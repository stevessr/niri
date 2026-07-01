### Overview

<sup>Since: next release</sup> niri has an experimental command-line helper for Miracast /
Wi-Fi Display. It can scan for sinks, establish the Wi-Fi Direct link, run the WFD RTSP source,
and launch a `wf-recorder` RTP/MPEG-TS media stream when the sink starts playback.

This helper talks to `wpa_supplicant` through `wpa_cli` and uses `wf-recorder` for Wayland screen
capture, so it requires:

- a Wi-Fi adapter and driver that support Wi-Fi Direct / P2P,
- `wpa_supplicant` with P2P control enabled,
- permission to access the `wpa_supplicant` control socket,
- `wf-recorder` with an H.264 encoder and RTP/MPEG-TS muxing support.

This is currently a low-level CLI workflow rather than a graphical picker.

### Typical Workflow

In one terminal, start the RTSP/media source:

```sh
niri miracast serve --output eDP-1
```

Then scan and connect from another terminal:

```sh
niri miracast scan --miracast-only
niri miracast connect aa:bb:cc:dd:ee:ff
```

When the sink connects to the RTSP source and sends `PLAY`, niri launches `wf-recorder` and sends
H.264 video over RTP/MPEG-TS to the sink-provided RTP port.

### Scan

Run:

```sh
niri miracast scan
```

If `wpa_supplicant` manages more than one Wi-Fi interface, pass the interface explicitly:

```sh
niri miracast scan --ifname wlan0
```

Machine-readable output is available:

```sh
niri miracast scan --json
```

Useful flags:

- `--timeout <seconds>`: discovery time, default 8 seconds.
- `--miracast-only`: hide peers that do not advertise Wi-Fi Display information.
- `--flush`: clear the P2P peer cache before scanning.
- `--no-wfd`: do not advertise niri as a Wi-Fi Display source before scanning.
- `--wfd-device-info <hex>`: override the Wi-Fi Display Device Information subelement passed to
  `wpa_cli wfd_subelem_set 0`.

### Connect

After finding a peer address in the scan output:

```sh
niri miracast connect aa:bb:cc:dd:ee:ff
```

By default this uses WPS PBC and asks `wpa_supplicant` to auto-detect whether it should join an
already-running Group Owner. Other useful options:

```sh
# Enter a PIN shown by the sink.
niri miracast connect aa:bb:cc:dd:ee:ff --pin 12345670

# Ask wpa_supplicant to generate a PIN to show to the sink.
niri miracast connect aa:bb:cc:dd:ee:ff --display-pin

# Request provision discovery first; useful for some sinks that need user approval.
niri miracast connect aa:bb:cc:dd:ee:ff --provdisc
```

Machine-readable output is available with `--json`.

### Serve and Stream

`niri miracast serve` listens for the Wi-Fi Display RTSP session, negotiates the mandatory WFD
parameters, and starts the media pipeline on `PLAY`.

Useful flags:

- `--bind <address>`: RTSP listen address, default `0.0.0.0`.
- `--port <port>`: RTSP listen port, default `7236`.
- `--accept-timeout <seconds>`: stop waiting for a sink after this many seconds; `0` means forever.
- `--session-timeout <seconds>`: stop an idle connected RTSP session after this many seconds; `0`
  means forever.
- `--output <name>`: niri output name passed to `wf-recorder --output`.
- `--framerate <fps>`: video frame rate, default `30`.
- `--bitrate-kbps <kbit/s>`: video bitrate, default `8000`.
- `--codec <codec>`: video codec for `wf-recorder`, default `libx264`.
- `--audio`: ask `wf-recorder` to include default audio and advertise AAC audio support.
- `--audio-device <name>`: ask `wf-recorder` to capture a specific PipeWire/Pulse source.
- `--no-media`: perform only the RTSP/WFD handshake without launching `wf-recorder`; useful for
  debugging.
- `--video-formats <string>`: override the selected WFD H.264 format string if your sink needs a
  different mode than the default 1280×720 at 30 Hz baseline profile.
- `--json`: print a machine-readable session summary after the session ends.

For example:

```sh
niri miracast serve --output HDMI-A-1 --framerate 30 --bitrate-kbps 12000
```

### Disconnect

To remove a P2P group interface:

```sh
niri miracast disconnect p2p-wlan0-0
```

Use the group interface name reported by `wpa_supplicant` or shown by tools such as `iw dev`.
