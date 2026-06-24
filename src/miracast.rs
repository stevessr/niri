//! Experimental Miracast / Wi-Fi Display command-line helper.
//!
//! This module intentionally keeps the first iteration small and system-integrated: it drives the
//! Wi-Fi Direct / WFD discovery and group-formation parts through `wpa_cli`, which is already the
//! standard control utility for `wpa_supplicant` P2P support.

use std::collections::BTreeMap;
use std::process::{Command as ProcessCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use serde::Serialize;

use crate::cli::MiracastCommand;

const DEFAULT_WPA_CLI_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MiracastPeer {
    pub address: String,
    pub name: Option<String>,
    pub is_miracast: bool,
    pub rtsp_port: Option<u16>,
    pub config_methods: Option<String>,
    pub device_type: Option<String>,
    pub manufacturer: Option<String>,
    pub model_name: Option<String>,
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MiracastConnectResult {
    pub peer: String,
    pub started: bool,
    pub group_ifname: Option<String>,
    pub wpa_cli_response: String,
    pub command: Vec<String>,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MiracastDisconnectResult {
    pub group_ifname: String,
    pub removed: bool,
    pub wpa_cli_response: String,
}

#[derive(Debug, Clone)]
struct WpaCli {
    ifname: Option<String>,
}

impl WpaCli {
    fn new(ifname: Option<String>) -> Self {
        Self { ifname }
    }

    fn run(&self, args: &[String], timeout: Duration) -> anyhow::Result<String> {
        let mut cmd = ProcessCommand::new("wpa_cli");
        if let Some(ifname) = &self.ifname {
            cmd.arg("-i").arg(ifname);
        }
        cmd.args(args);
        run_with_timeout(cmd, timeout).with_context(|| {
            let mut rendered = String::from("wpa_cli");
            if let Some(ifname) = &self.ifname {
                rendered.push_str(" -i ");
                rendered.push_str(ifname);
            }
            for arg in args {
                rendered.push(' ');
                rendered.push_str(arg);
            }
            format!("error running `{rendered}`")
        })
    }

    fn run_ok(&self, args: &[String], timeout: Duration) -> anyhow::Result<String> {
        let output = self.run(args, timeout)?;
        if is_wpa_cli_failure(&output) {
            bail!("wpa_cli returned failure: {}", output.trim());
        }
        Ok(output)
    }
}

pub fn handle_miracast(command: MiracastCommand) -> anyhow::Result<()> {
    match command {
        MiracastCommand::Scan {
            ifname,
            timeout,
            json,
            miracast_only,
            flush,
            no_wfd,
            wfd_device_info,
        } => {
            validate_wfd_device_info(&wfd_device_info)?;

            let cli = WpaCli::new(ifname);
            let timeout = Duration::from_secs(timeout);
            let peers = scan(
                &cli,
                timeout,
                miracast_only,
                flush,
                no_wfd,
                &wfd_device_info,
            )?;

            if json {
                println!("{}", serde_json::to_string_pretty(&peers)?);
            } else {
                print_scan(peers);
            }
        }
        MiracastCommand::Connect {
            peer,
            ifname,
            timeout,
            json,
            pin,
            display_pin,
            persistent,
            join,
            provdisc,
            no_auto,
            go_intent,
            freq,
            no_wfd,
            wfd_device_info,
        } => {
            validate_peer_address(&peer)?;
            validate_wfd_device_info(&wfd_device_info)?;
            if let Some(go_intent) = go_intent {
                if go_intent > 15 {
                    bail!("--go-intent must be between 0 and 15");
                }
            }

            let cli = WpaCli::new(ifname);
            let result = connect(
                &cli,
                ConnectOptions {
                    peer,
                    timeout: Duration::from_secs(timeout),
                    pin,
                    display_pin,
                    persistent,
                    join,
                    provdisc,
                    auto: !no_auto,
                    go_intent,
                    freq,
                    no_wfd,
                    wfd_device_info,
                },
            )?;

            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_connect(result);
            }
        }
        MiracastCommand::Disconnect {
            group_ifname,
            ifname,
            json,
        } => {
            let cli = WpaCli::new(ifname);
            let result = disconnect(&cli, group_ifname)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                println!("Removed P2P group interface {}.", result.group_ifname);
            }
        }
    }

    Ok(())
}

fn scan(
    cli: &WpaCli,
    timeout: Duration,
    miracast_only: bool,
    flush: bool,
    no_wfd: bool,
    wfd_device_info: &str,
) -> anyhow::Result<Vec<MiracastPeer>> {
    if !no_wfd {
        configure_wfd_source(cli, wfd_device_info)?;
    }

    if flush {
        let args = strings(["p2p_flush"]);
        cli.run_ok(&args, DEFAULT_WPA_CLI_TIMEOUT)?;
    }

    if !timeout.is_zero() {
        let timeout_arg = timeout.as_secs().to_string();
        let args = strings(["p2p_find", timeout_arg.as_str()]);
        cli.run_ok(&args, DEFAULT_WPA_CLI_TIMEOUT)?;
        thread::sleep(timeout);

        let args = strings(["p2p_stop_find"]);
        // `p2p_stop_find` can fail when discovery already timed out; scanning itself still
        // succeeded, so ignore this cleanup error.
        let _ = cli.run(&args, DEFAULT_WPA_CLI_TIMEOUT);
    }

    let args = strings(["p2p_peers", "discovered"]);
    let peers_output = cli.run_ok(&args, DEFAULT_WPA_CLI_TIMEOUT)?;
    let mut peers = Vec::new();
    for address in parse_peer_addresses(&peers_output) {
        let args = strings(["p2p_peer", &address]);
        let details = cli.run_ok(&args, DEFAULT_WPA_CLI_TIMEOUT)?;
        let peer = parse_peer_details(&address, &details);
        if !miracast_only || peer.is_miracast {
            peers.push(peer);
        }
    }

    peers.sort_by(|a, b| {
        a.name
            .as_deref()
            .unwrap_or("")
            .cmp(b.name.as_deref().unwrap_or(""))
            .then_with(|| a.address.cmp(&b.address))
    });
    Ok(peers)
}

struct ConnectOptions {
    peer: String,
    timeout: Duration,
    pin: Option<String>,
    display_pin: bool,
    persistent: bool,
    join: bool,
    provdisc: bool,
    auto: bool,
    go_intent: Option<u8>,
    freq: Option<u32>,
    no_wfd: bool,
    wfd_device_info: String,
}

fn connect(cli: &WpaCli, options: ConnectOptions) -> anyhow::Result<MiracastConnectResult> {
    if !options.no_wfd {
        configure_wfd_source(cli, &options.wfd_device_info)?;
    }

    let interfaces_before = list_interfaces(cli).unwrap_or_default();

    let args = strings(["p2p_stop_find"]);
    let _ = cli.run(&args, DEFAULT_WPA_CLI_TIMEOUT);

    let command = build_connect_command(&options);
    let response = cli.run_ok(&command, DEFAULT_WPA_CLI_TIMEOUT)?;
    let started = !is_wpa_cli_failure(&response);
    let group_ifname = if started {
        wait_for_group_interface(cli, &interfaces_before, options.timeout)
    } else {
        None
    };

    Ok(MiracastConnectResult {
        peer: options.peer,
        started,
        group_ifname,
        wpa_cli_response: response.trim().to_owned(),
        command,
        note: String::from(
            "Wi-Fi Direct group formation was submitted to wpa_supplicant. \
             Complete Miracast media streaming still depends on the sink accepting the P2P link \
             and on a WFD/RTSP/RTP media pipeline.",
        ),
    })
}

fn list_interfaces(cli: &WpaCli) -> anyhow::Result<Vec<String>> {
    let args = strings(["interface"]);
    let output = cli.run_ok(&args, DEFAULT_WPA_CLI_TIMEOUT)?;
    Ok(parse_interfaces(&output))
}

fn wait_for_group_interface(
    cli: &WpaCli,
    interfaces_before: &[String],
    timeout: Duration,
) -> Option<String> {
    let deadline = Instant::now() + timeout;

    loop {
        if let Ok(interfaces) = list_interfaces(cli) {
            if let Some(group) = interfaces.into_iter().find(|ifname| {
                !interfaces_before.contains(ifname)
                    && (ifname.starts_with("p2p-") || ifname.starts_with("DIRECT-"))
            }) {
                return Some(group);
            }
        }

        if Instant::now() >= deadline {
            return None;
        }

        thread::sleep(Duration::from_millis(500));
    }
}

fn disconnect(cli: &WpaCli, group_ifname: String) -> anyhow::Result<MiracastDisconnectResult> {
    let args = strings(["p2p_group_remove", &group_ifname]);
    let response = cli.run_ok(&args, DEFAULT_WPA_CLI_TIMEOUT)?;

    Ok(MiracastDisconnectResult {
        group_ifname,
        removed: !is_wpa_cli_failure(&response),
        wpa_cli_response: response.trim().to_owned(),
    })
}

fn build_connect_command(options: &ConnectOptions) -> Vec<String> {
    let mut command = vec![String::from("p2p_connect"), options.peer.clone()];

    if options.display_pin {
        command.push(String::from("pin"));
        command.push(String::from("display"));
    } else if let Some(pin) = &options.pin {
        command.push(pin.clone());
        command.push(String::from("keypad"));
    } else {
        command.push(String::from("pbc"));
    }

    if options.persistent {
        command.push(String::from("persistent"));
    }
    if options.join {
        command.push(String::from("join"));
    }
    if options.provdisc {
        command.push(String::from("provdisc"));
    }
    if options.auto {
        command.push(String::from("auto"));
    }
    if let Some(go_intent) = options.go_intent {
        command.push(format!("go_intent={go_intent}"));
    }
    if let Some(freq) = options.freq {
        command.push(format!("freq={freq}"));
    }

    command
}

fn configure_wfd_source(cli: &WpaCli, device_info: &str) -> anyhow::Result<()> {
    let args = strings(["wfd_subelem_set", "0", device_info]);
    cli.run_ok(&args, DEFAULT_WPA_CLI_TIMEOUT)
        .with_context(|| {
            "error advertising Wi-Fi Display source capabilities; try --no-wfd if this \
         wpa_supplicant build does not support WFD"
        })?;
    Ok(())
}

fn run_with_timeout(mut cmd: ProcessCommand, timeout: Duration) -> anyhow::Result<String> {
    let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    let deadline = Instant::now() + timeout;

    loop {
        if child.try_wait()?.is_some() {
            let output = child.wait_with_output()?;
            if !output.status.success() {
                bail!(
                    "command exited with {}: {}{}",
                    output.status,
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }

            return String::from_utf8(output.stdout).context("wpa_cli returned invalid UTF-8");
        }

        if Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output()?;
            bail!(
                "command timed out after {}s: {}{}",
                timeout.as_secs(),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        thread::sleep(Duration::from_millis(50));
    }
}

fn parse_peer_addresses(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| is_mac_address(line))
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_interfaces(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("Selected interface")
                && *line != "Available interfaces:"
        })
        .map(ToOwned::to_owned)
        .collect()
}

fn parse_peer_details(address: &str, output: &str) -> MiracastPeer {
    let mut fields = BTreeMap::new();

    for line in output.lines().map(str::trim) {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        fields.insert(key.trim().to_owned(), value.trim().to_owned());
    }

    let name = first_field(&fields, &["device_name", "name"]).map(ToOwned::to_owned);
    let rtsp_port = first_field(&fields, &["wfd_rtsp_ctrlport", "wfd_rtsp_port"])
        .and_then(|port| port.parse().ok())
        .or_else(|| {
            fields
                .get("wfd_subelems")
                .and_then(|subelements| parse_wfd_rtsp_port(subelements))
        });
    let is_miracast = fields.keys().any(|key| key.starts_with("wfd_"))
        || fields
            .get("wfd_subelems")
            .is_some_and(|value| !value.is_empty());

    MiracastPeer {
        address: address.to_owned(),
        name,
        is_miracast,
        rtsp_port,
        config_methods: fields.get("config_methods").cloned(),
        device_type: first_field(&fields, &["pri_dev_type", "device_type"]).map(ToOwned::to_owned),
        manufacturer: fields.get("manufacturer").cloned(),
        model_name: fields.get("model_name").cloned(),
        fields,
    }
}

fn parse_wfd_rtsp_port(hex: &str) -> Option<u16> {
    let bytes = decode_hex(hex)?;

    let mut idx = 0;
    while idx + 3 <= bytes.len() {
        let subelement = bytes[idx];
        let len = u16::from_be_bytes([bytes[idx + 1], bytes[idx + 2]]) as usize;
        idx += 3;
        if idx + len > bytes.len() {
            break;
        }

        if subelement == 0 && len >= 4 {
            return Some(u16::from_be_bytes([bytes[idx + 2], bytes[idx + 3]]));
        }

        idx += len;
    }

    // Some tools print subelement 0 without the one-byte id but keep the two-byte length prefix.
    if bytes.len() >= 6 {
        let len = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        if len == bytes.len() - 2 && len >= 4 {
            return Some(u16::from_be_bytes([bytes[4], bytes[5]]));
        }
    }

    // Other tools print only the WFD Device Information payload. In that form the first two bytes
    // are the WFD Device Information bitmap, followed by the RTSP port.
    if bytes.len() >= 4 {
        return Some(u16::from_be_bytes([bytes[2], bytes[3]]));
    }

    None
}

fn first_field<'a>(fields: &'a BTreeMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| fields.get(*key).map(String::as_str))
}

fn is_wpa_cli_failure(output: &str) -> bool {
    let trimmed = output.trim();
    trimmed == "FAIL" || trimmed.starts_with("FAIL\n")
}

fn validate_peer_address(address: &str) -> anyhow::Result<()> {
    if is_mac_address(address) {
        Ok(())
    } else {
        Err(anyhow!(
            "peer must be a P2P device MAC address, got `{address}`"
        ))
    }
}

fn validate_wfd_device_info(device_info: &str) -> anyhow::Result<()> {
    if device_info.len() % 2 != 0 {
        bail!("WFD device info must have an even number of hex digits");
    }
    if !device_info.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("WFD device info must contain only hex digits");
    }
    Ok(())
}

fn is_mac_address(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 17
        && bytes.iter().enumerate().all(|(idx, b)| {
            if idx % 3 == 2 {
                *b == b':'
            } else {
                b.is_ascii_hexdigit()
            }
        })
}

fn decode_hex(hex: &str) -> Option<Vec<u8>> {
    if hex.len() % 2 != 0 {
        return None;
    }

    let mut out = Vec::with_capacity(hex.len() / 2);
    for idx in (0..hex.len()).step_by(2) {
        let byte = u8::from_str_radix(&hex[idx..idx + 2], 16).ok()?;
        out.push(byte);
    }
    Some(out)
}

fn strings<const N: usize>(items: [&str; N]) -> Vec<String> {
    items.into_iter().map(ToOwned::to_owned).collect()
}

fn print_scan(peers: Vec<MiracastPeer>) {
    if peers.is_empty() {
        println!("No Wi-Fi Direct peers found.");
        return;
    }

    for peer in peers {
        println!(
            "{}{}",
            peer.address,
            peer.name
                .as_deref()
                .map(|name| format!("  {name}"))
                .unwrap_or_default()
        );
        println!(
            "  Miracast/Wi-Fi Display: {}",
            if peer.is_miracast {
                "yes"
            } else {
                "unknown/no"
            }
        );
        if let Some(port) = peer.rtsp_port {
            println!("  RTSP control port: {port}");
        }
        if let Some(config_methods) = &peer.config_methods {
            println!("  Config methods: {config_methods}");
        }
        if let Some(device_type) = &peer.device_type {
            println!("  Device type: {device_type}");
        }
        if let Some(model_name) = &peer.model_name {
            println!("  Model: {model_name}");
        }
        println!();
    }
}

fn print_connect(result: MiracastConnectResult) {
    println!(
        "Submitted Miracast/Wi-Fi Direct link request to {}.",
        result.peer
    );
    println!("wpa_cli response: {}", result.wpa_cli_response);
    if let Some(group_ifname) = &result.group_ifname {
        println!("P2P group interface: {group_ifname}");
    } else {
        println!("P2P group interface was not observed before timeout.");
    }
    println!();
    println!("{}", result.note);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_peer_addresses() {
        let peers = parse_peer_addresses(
            r#"
            aa:bb:cc:dd:ee:ff
            selected interface 'wlan0'
            12:34:56:78:9a:bc
            "#,
        );

        assert_eq!(peers, ["aa:bb:cc:dd:ee:ff", "12:34:56:78:9a:bc"]);
    }

    #[test]
    fn parses_wpa_cli_interfaces() {
        let interfaces = parse_interfaces(
            r#"
            Selected interface 'wlan0'
            Available interfaces:
            wlan0
            p2p-dev-wlan0
            p2p-wlan0-0
            "#,
        );

        assert_eq!(interfaces, ["wlan0", "p2p-dev-wlan0", "p2p-wlan0-0"]);
    }

    #[test]
    fn parses_wfd_peer_details() {
        let peer = parse_peer_details(
            "aa:bb:cc:dd:ee:ff",
            r#"
            device_name=Living Room TV
            pri_dev_type=7-0050F204-1
            config_methods=0x188
            manufacturer=Example
            model_name=Sink
            wfd_rtsp_ctrlport=7236
            wfd_session_avail=1
            "#,
        );

        assert_eq!(peer.name.as_deref(), Some("Living Room TV"));
        assert!(peer.is_miracast);
        assert_eq!(peer.rtsp_port, Some(7236));
        assert_eq!(peer.model_name.as_deref(), Some("Sink"));
    }

    #[test]
    fn parses_rtsp_port_from_wfd_subelements() {
        // Subelement id 0, length 6, WFD device info 0x0011, RTSP port 7236,
        // throughput 50.
        assert_eq!(parse_wfd_rtsp_port("00000600111c440032"), Some(7236));
        // `wfd_subelem_get 0` omits the subelement id but keeps the length.
        assert_eq!(parse_wfd_rtsp_port("000600111c440032"), Some(7236));
        // Some command outputs omit both the subelement id and length.
        assert_eq!(parse_wfd_rtsp_port("00111c440032"), Some(7236));
    }

    #[test]
    fn builds_pbc_connect_command() {
        let options = ConnectOptions {
            peer: String::from("aa:bb:cc:dd:ee:ff"),
            timeout: Duration::from_secs(1),
            pin: None,
            display_pin: false,
            persistent: true,
            join: false,
            provdisc: true,
            auto: true,
            go_intent: Some(0),
            freq: Some(2412),
            no_wfd: false,
            wfd_device_info: String::from("000600111c440032"),
        };

        assert_eq!(
            build_connect_command(&options),
            [
                "p2p_connect",
                "aa:bb:cc:dd:ee:ff",
                "pbc",
                "persistent",
                "provdisc",
                "auto",
                "go_intent=0",
                "freq=2412"
            ]
        );
    }

    #[test]
    fn builds_pin_connect_command() {
        let options = ConnectOptions {
            peer: String::from("aa:bb:cc:dd:ee:ff"),
            timeout: Duration::from_secs(1),
            pin: Some(String::from("12345670")),
            display_pin: false,
            persistent: false,
            join: true,
            provdisc: false,
            auto: false,
            go_intent: None,
            freq: None,
            no_wfd: false,
            wfd_device_info: String::from("000600111c440032"),
        };

        assert_eq!(
            build_connect_command(&options),
            [
                "p2p_connect",
                "aa:bb:cc:dd:ee:ff",
                "12345670",
                "keypad",
                "join"
            ]
        );
    }
}
