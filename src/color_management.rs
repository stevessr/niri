use std::fs;
use std::path::Path;

use anyhow::{bail, ensure, Context};

const ICC_HEADER_LEN: usize = 128;
const TAG_TABLE_HEADER_LEN: usize = 4;
const TAG_RECORD_LEN: usize = 12;
const VCGT_TABLE_TYPE: u32 = 0;
const VCGT_FORMULA_TYPE: u32 = 1;

/// Load the display calibration stored in an ICC profile's `vcgt` tag and resample it to the
/// hardware LUT size expected by the DRM gamma API.
///
/// This is calibration only. It does not perform ICC characterization transforms or HDR tone
/// mapping.
pub fn load_vcgt(path: &Path, output_entries: usize) -> anyhow::Result<Vec<u16>> {
    let data = fs::read(path).with_context(|| format!("error reading ICC profile {}", path.display()))?;
    parse_vcgt(&data, output_entries)
        .with_context(|| format!("error parsing ICC profile {}", path.display()))
}

fn parse_vcgt(data: &[u8], output_entries: usize) -> anyhow::Result<Vec<u16>> {
    ensure!(output_entries > 0, "output gamma LUT has no entries");
    ensure!(
        data.len() >= ICC_HEADER_LEN + TAG_TABLE_HEADER_LEN,
        "ICC profile is too short"
    );
    ensure!(&data[36..40] == b"acsp", "missing ICC profile signature");

    let declared_size = be_u32(data, 0)? as usize;
    ensure!(
        declared_size >= ICC_HEADER_LEN + TAG_TABLE_HEADER_LEN,
        "invalid ICC profile size"
    );
    ensure!(
        declared_size <= data.len(),
        "ICC profile is truncated: header declares {declared_size} bytes, file has {}",
        data.len()
    );
    let data = &data[..declared_size];

    let tag_count = be_u32(data, ICC_HEADER_LEN)? as usize;
    let table_len = tag_count
        .checked_mul(TAG_RECORD_LEN)
        .and_then(|x| x.checked_add(ICC_HEADER_LEN + TAG_TABLE_HEADER_LEN))
        .context("ICC tag table size overflow")?;
    ensure!(table_len <= data.len(), "ICC tag table is truncated");

    let mut vcgt = None;
    for idx in 0..tag_count {
        let record = ICC_HEADER_LEN + TAG_TABLE_HEADER_LEN + idx * TAG_RECORD_LEN;
        if &data[record..record + 4] != b"vcgt" {
            continue;
        }

        let offset = be_u32(data, record + 4)? as usize;
        let size = be_u32(data, record + 8)? as usize;
        let end = offset.checked_add(size).context("vcgt tag size overflow")?;
        ensure!(size >= 12, "vcgt tag is too short");
        ensure!(end <= data.len(), "vcgt tag extends past the ICC profile");
        vcgt = Some(&data[offset..end]);
        break;
    }

    let vcgt = vcgt.context("ICC profile does not contain a vcgt calibration tag")?;
    ensure!(&vcgt[..4] == b"vcgt", "vcgt tag has the wrong type signature");

    match be_u32(vcgt, 8)? {
        VCGT_TABLE_TYPE => parse_table_vcgt(vcgt, output_entries),
        VCGT_FORMULA_TYPE => parse_formula_vcgt(vcgt, output_entries),
        tag_type => bail!("unsupported vcgt storage type {tag_type}"),
    }
}

fn parse_table_vcgt(vcgt: &[u8], output_entries: usize) -> anyhow::Result<Vec<u16>> {
    ensure!(vcgt.len() >= 18, "vcgt table header is truncated");

    let channels = be_u16(vcgt, 12)? as usize;
    let entry_count = be_u16(vcgt, 14)? as usize;
    let entry_size = be_u16(vcgt, 16)? as usize;

    ensure!(matches!(channels, 1 | 3), "vcgt channel count must be 1 or 3");
    ensure!(entry_count >= 2, "vcgt table must contain at least two entries");
    ensure!(matches!(entry_size, 1 | 2), "vcgt entry size must be 1 or 2 bytes");

    let value_count = channels
        .checked_mul(entry_count)
        .context("vcgt table entry count overflow")?;
    let byte_count = value_count
        .checked_mul(entry_size)
        .context("vcgt table size overflow")?;
    let end = 18usize
        .checked_add(byte_count)
        .context("vcgt table size overflow")?;
    ensure!(end <= vcgt.len(), "vcgt table data is truncated");

    let mut parsed = Vec::with_capacity(value_count);
    let table = &vcgt[18..end];
    match entry_size {
        1 => parsed.extend(table.iter().map(|&x| u16::from(x) * 257)),
        2 => {
            for chunk in table.chunks_exact(2) {
                parsed.push(u16::from_be_bytes([chunk[0], chunk[1]]));
            }
        }
        _ => unreachable!(),
    }

    if channels == 1 {
        let channel = resample(&parsed, output_entries);
        let mut ramp = Vec::with_capacity(output_entries * 3);
        ramp.extend_from_slice(&channel);
        ramp.extend_from_slice(&channel);
        ramp.extend_from_slice(&channel);
        return Ok(ramp);
    }

    let red = resample(&parsed[..entry_count], output_entries);
    let green = resample(&parsed[entry_count..entry_count * 2], output_entries);
    let blue = resample(&parsed[entry_count * 2..entry_count * 3], output_entries);

    let mut ramp = Vec::with_capacity(output_entries * 3);
    ramp.extend_from_slice(&red);
    ramp.extend_from_slice(&green);
    ramp.extend_from_slice(&blue);
    Ok(ramp)
}

fn parse_formula_vcgt(vcgt: &[u8], output_entries: usize) -> anyhow::Result<Vec<u16>> {
    // ColorSync stores nine signed 16.16 fixed-point values after the tag type:
    // gamma, minimum and maximum for red, green and blue.
    ensure!(vcgt.len() >= 48, "vcgt formula data is truncated");

    let mut values = [0.0; 9];
    for (idx, value) in values.iter_mut().enumerate() {
        let offset = 12 + idx * 4;
        let raw = i32::from_be_bytes(
            vcgt[offset..offset + 4]
                .try_into()
                .expect("fixed-size slice"),
        );
        *value = f64::from(raw) / 65536.0;
    }

    let mut ramp = Vec::with_capacity(output_entries * 3);
    for channel in 0..3 {
        let gamma = values[channel * 3];
        let min = values[channel * 3 + 1];
        let max = values[channel * 3 + 2];

        ensure!(gamma > 0.0 && gamma.is_finite(), "invalid vcgt gamma value");
        ensure!(
            min.is_finite() && max.is_finite() && min >= 0.0 && max <= 1.0 && min <= max,
            "invalid vcgt minimum/maximum values"
        );

        for idx in 0..output_entries {
            let x = if output_entries == 1 {
                0.0
            } else {
                idx as f64 / (output_entries - 1) as f64
            };
            let y = (min + (max - min) * x.powf(gamma)).clamp(0.0, 1.0);
            ramp.push((y * f64::from(u16::MAX)).round() as u16);
        }
    }

    Ok(ramp)
}

fn resample(input: &[u16], output_entries: usize) -> Vec<u16> {
    if output_entries == input.len() {
        return input.to_vec();
    }
    if output_entries == 1 {
        return vec![input[0]];
    }

    let src_last = input.len() - 1;
    let dst_last = output_entries - 1;
    (0..output_entries)
        .map(|idx| {
            let position = idx as f64 * src_last as f64 / dst_last as f64;
            let lo = position.floor() as usize;
            let hi = position.ceil() as usize;
            if lo == hi {
                input[lo]
            } else {
                let amount = position - lo as f64;
                (f64::from(input[lo]) * (1.0 - amount) + f64::from(input[hi]) * amount).round()
                    as u16
            }
        })
        .collect()
}

fn be_u16(data: &[u8], offset: usize) -> anyhow::Result<u16> {
    let end = offset.checked_add(2).context("offset overflow")?;
    let bytes: [u8; 2] = data
        .get(offset..end)
        .context("unexpected end of ICC data")?
        .try_into()
        .expect("fixed-size slice");
    Ok(u16::from_be_bytes(bytes))
}

fn be_u32(data: &[u8], offset: usize) -> anyhow::Result<u32> {
    let end = offset.checked_add(4).context("offset overflow")?;
    let bytes: [u8; 4] = data
        .get(offset..end)
        .context("unexpected end of ICC data")?
        .try_into()
        .expect("fixed-size slice");
    Ok(u32::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_with_vcgt(tag: Vec<u8>) -> Vec<u8> {
        let offset = 144usize;
        let size = offset + tag.len();
        let mut profile = vec![0; size];
        profile[0..4].copy_from_slice(&(size as u32).to_be_bytes());
        profile[36..40].copy_from_slice(b"acsp");
        profile[128..132].copy_from_slice(&1u32.to_be_bytes());
        profile[132..136].copy_from_slice(b"vcgt");
        profile[136..140].copy_from_slice(&(offset as u32).to_be_bytes());
        profile[140..144].copy_from_slice(&(tag.len() as u32).to_be_bytes());
        profile[offset..].copy_from_slice(&tag);
        profile
    }

    fn table_tag(channels: u16, values: &[u16]) -> Vec<u8> {
        let entries = values.len() / channels as usize;
        let mut tag = vec![0; 18 + values.len() * 2];
        tag[0..4].copy_from_slice(b"vcgt");
        tag[8..12].copy_from_slice(&VCGT_TABLE_TYPE.to_be_bytes());
        tag[12..14].copy_from_slice(&channels.to_be_bytes());
        tag[14..16].copy_from_slice(&(entries as u16).to_be_bytes());
        tag[16..18].copy_from_slice(&2u16.to_be_bytes());
        for (idx, value) in values.iter().enumerate() {
            let offset = 18 + idx * 2;
            tag[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
        }
        tag
    }

    #[test]
    fn parses_and_resamples_three_channel_table() {
        let tag = table_tag(
            3,
            &[0, 65535, 0, 32768, 0, 16384],
        );
        let ramp = parse_vcgt(&profile_with_vcgt(tag), 3).unwrap();

        assert_eq!(&ramp[0..3], &[0, 32768, 65535]);
        assert_eq!(&ramp[3..6], &[0, 16384, 32768]);
        assert_eq!(&ramp[6..9], &[0, 8192, 16384]);
    }

    #[test]
    fn duplicates_single_channel_table() {
        let tag = table_tag(1, &[0, 65535]);
        let ramp = parse_vcgt(&profile_with_vcgt(tag), 4).unwrap();

        assert_eq!(&ramp[0..4], &ramp[4..8]);
        assert_eq!(&ramp[4..8], &ramp[8..12]);
        assert_eq!(ramp[0], 0);
        assert_eq!(ramp[3], 65535);
    }

    #[test]
    fn rejects_missing_vcgt() {
        let mut profile = vec![0; 132];
        profile[0..4].copy_from_slice(&132u32.to_be_bytes());
        profile[36..40].copy_from_slice(b"acsp");
        profile[128..132].copy_from_slice(&0u32.to_be_bytes());

        assert!(parse_vcgt(&profile, 256).is_err());
    }

    #[test]
    fn rejects_truncated_table() {
        let mut tag = table_tag(1, &[0, 65535]);
        tag.pop();
        assert!(parse_vcgt(&profile_with_vcgt(tag), 256).is_err());
    }
}
