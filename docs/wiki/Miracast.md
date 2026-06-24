### Overview

<sup>Since: next release</sup> niri has an experimental command-line helper for the Wi-Fi Direct
discovery and link setup parts of Miracast / Wi-Fi Display.

This helper talks to `wpa_supplicant` through `wpa_cli`, so it requires:

- a Wi-Fi adapter and driver that support Wi-Fi Direct / P2P,
- `wpa_supplicant` with P2P control enabled,
- permission to access the `wpa_supplicant` control socket.

This is currently a low-level CLI workflow rather than a graphical picker.

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

### Disconnect

To remove a P2P group interface:

```sh
niri miracast disconnect p2p-wlan0-0
```

Use the group interface name reported by `wpa_supplicant` or shown by tools such as `iw dev`.
