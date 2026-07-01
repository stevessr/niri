//! Experimental Miracast / Wi-Fi Display command-line helper.
//!
//! This module intentionally keeps the first iteration small and system-integrated: it drives the
//! Wi-Fi Direct / WFD discovery and group-formation parts through `wpa_cli`, which is already the
//! standard control utility for `wpa_supplicant` P2P support.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Command as ProcessCommand, Stdio};
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MiracastServeResult {
    pub peer: String,
    pub presentation_url: String,
    pub sink_rtp_port: Option<u16>,
    pub media_started: bool,
    pub media_command: Option<Vec<String>>,
    pub teardown_received: bool,
}

#[derive(Debug, Clone)]
struct RtspSourceConfig {
    bind: String,
    port: u16,
    accept_timeout: Option<Duration>,
    session_timeout: Option<Duration>,
    output: Option<String>,
    framerate: u32,
    bitrate_kbps: u32,
    codec: String,
    audio: Option<Option<String>>,
    no_media: bool,
    video_formats: String,
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
        MiracastCommand::Serve {
            bind,
            port,
            accept_timeout,
            session_timeout,
            output,
            framerate,
            bitrate_kbps,
            codec,
            audio,
            audio_device,
            no_media,
            video_formats,
            json,
        } => {
            validate_video_formats(&video_formats)?;

            let result = serve_rtsp_source(RtspSourceConfig {
                bind,
                port,
                accept_timeout: duration_arg(accept_timeout),
                session_timeout: duration_arg(session_timeout),
                output,
                framerate,
                bitrate_kbps,
                codec,
                audio: audio_device.map(Some).or(audio.then_some(None)),
                no_media,
                video_formats,
            })?;

            if json {
                println!("{}", serde_json::to_string_pretty(&result)?);
            } else {
                print_serve(result);
            }
        }
    }

    Ok(())
}

fn duration_arg(seconds: u64) -> Option<Duration> {
    (seconds != 0).then(|| Duration::from_secs(seconds))
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
             Keep `niri miracast serve` running so the sink can complete the \
             WFD/RTSP control session and receive the RTP/MPEG-TS media stream.",
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

fn serve_rtsp_source(config: RtspSourceConfig) -> anyhow::Result<MiracastServeResult> {
    let listener = TcpListener::bind((config.bind.as_str(), config.port)).with_context(|| {
        format!(
            "error binding RTSP listener on {}:{}",
            config.bind, config.port
        )
    })?;
    listener
        .set_nonblocking(true)
        .context("error setting RTSP listener non-blocking")?;

    eprintln!(
        "Waiting for Miracast sink RTSP connection on {}:{} ...",
        config.bind, config.port
    );
    let deadline = config
        .accept_timeout
        .map(|timeout| Instant::now() + timeout);
    let (stream, peer_addr) = loop {
        match listener.accept() {
            Ok((stream, peer_addr)) => break (stream, peer_addr),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    bail!("timed out waiting for Miracast sink RTSP connection");
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(err).context("error accepting RTSP connection"),
        }
    };

    run_rtsp_session(stream, peer_addr, config)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutgoingRtsp {
    Options,
    GetParameters,
    SetParameters,
    TriggerSetup,
}

#[derive(Debug)]
struct OutgoingRtspState {
    next_cseq: u32,
    pending: BTreeMap<u32, OutgoingRtsp>,
}

#[derive(Debug)]
enum RtspMessage {
    Request {
        method: String,
        headers: BTreeMap<String, String>,
        body: String,
    },
    Response {
        code: u16,
        headers: BTreeMap<String, String>,
        body: String,
    },
}

fn run_rtsp_session(
    mut stream: TcpStream,
    peer_addr: SocketAddr,
    config: RtspSourceConfig,
) -> anyhow::Result<MiracastServeResult> {
    stream
        .set_read_timeout(config.session_timeout)
        .context("error setting RTSP read timeout")?;
    stream
        .set_write_timeout(Some(DEFAULT_WPA_CLI_TIMEOUT))
        .context("error setting RTSP write timeout")?;

    let local_addr = stream
        .local_addr()
        .context("error getting RTSP local address")?;
    let presentation_url = format!(
        "rtsp://{}:{}/wfd1.0/streamid=0",
        local_addr.ip(),
        local_addr.port()
    );

    eprintln!("Miracast sink connected from {peer_addr}.");

    let reader_stream = stream.try_clone().context("error cloning RTSP stream")?;
    let mut reader = BufReader::new(reader_stream);
    let mut outgoing = OutgoingRtspState {
        next_cseq: 1,
        pending: BTreeMap::new(),
    };
    let mut session_id = String::from("niri");
    let mut sink_rtp_port = None;
    let mut media = None;
    let mut media_command = None;
    let mut teardown_received = false;

    send_rtsp_request(
        &mut stream,
        &mut outgoing,
        OutgoingRtsp::Options,
        "OPTIONS",
        "*",
        &[("Require", "org.wfa.wfd1.0")],
        "",
    )?;

    loop {
        let Some(message) = read_rtsp_message(&mut reader)? else {
            break;
        };

        match message {
            RtspMessage::Response {
                code,
                headers,
                body,
            } => {
                if !(200..300).contains(&code) {
                    bail!("Miracast sink returned RTSP error {code}: {body}");
                }

                let Some(cseq) = header(&headers, "cseq").and_then(|value| value.parse().ok())
                else {
                    continue;
                };
                let Some(kind) = outgoing.pending.remove(&cseq) else {
                    continue;
                };

                match kind {
                    OutgoingRtsp::Options => {
                        send_m3_get_parameters(&mut stream, &mut outgoing)?;
                    }
                    OutgoingRtsp::GetParameters => {
                        let params = parse_rtsp_parameters(&body);
                        if let Some(port) = parse_wfd_client_rtp_port_from_params(&params) {
                            sink_rtp_port = Some(port);
                        }
                        send_m4_set_parameters(
                            &mut stream,
                            &mut outgoing,
                            &config,
                            &presentation_url,
                            sink_rtp_port,
                        )?;
                    }
                    OutgoingRtsp::SetParameters => {
                        send_trigger_setup(&mut stream, &mut outgoing)?;
                    }
                    OutgoingRtsp::TriggerSetup => (),
                }
            }
            RtspMessage::Request {
                method,
                headers,
                body,
            } => {
                let cseq = header(&headers, "cseq")
                    .cloned()
                    .unwrap_or_else(|| String::from("0"));

                match method.as_str() {
                    "OPTIONS" => {
                        send_rtsp_response(
                            &mut stream,
                            &cseq,
                            &[
                                (
                                    "Public",
                                    "org.wfa.wfd1.0, OPTIONS, GET_PARAMETER, SET_PARAMETER, SETUP, PLAY, PAUSE, TEARDOWN",
                                ),
                            ],
                            "",
                        )?;
                    }
                    "GET_PARAMETER" => {
                        let body = build_get_parameter_response(
                            &body,
                            &config,
                            sink_rtp_port,
                            &presentation_url,
                        );
                        send_rtsp_response(
                            &mut stream,
                            &cseq,
                            &[("Content-Type", "text/parameters")],
                            &body,
                        )?;
                    }
                    "SET_PARAMETER" => {
                        let params = parse_rtsp_parameters(&body);
                        let response_body = params.keys().cloned().collect::<Vec<_>>().join("\r\n");
                        let response_body = if response_body.is_empty() {
                            String::new()
                        } else {
                            format!("{response_body}\r\n")
                        };
                        send_rtsp_response(
                            &mut stream,
                            &cseq,
                            &[("Content-Type", "text/parameters")],
                            &response_body,
                        )?;
                    }
                    "SETUP" => {
                        if let Some(transport) = header(&headers, "transport") {
                            if let Some(port) = parse_transport_client_port(transport) {
                                sink_rtp_port = Some(port);
                            }
                        }
                        session_id = format!("niri{:08x}", fastrand::u32(..));
                        let server_port = 50000 + fastrand::u16(0..1000) * 2;
                        let transport = format!(
                            "RTP/AVP/UDP;unicast;client_port={};server_port={}-{}",
                            sink_rtp_port.unwrap_or(19000),
                            server_port,
                            server_port + 1
                        );
                        send_rtsp_response(
                            &mut stream,
                            &cseq,
                            &[("Session", &session_id), ("Transport", &transport)],
                            "",
                        )?;
                    }
                    "PLAY" => {
                        send_rtsp_response(&mut stream, &cseq, &[("Session", &session_id)], "")?;
                        if media.is_none() && !config.no_media {
                            let port = sink_rtp_port
                                .ok_or_else(|| anyhow!("sink did not provide an RTP port"))?;
                            let args =
                                build_wf_recorder_args(&config, &peer_addr.ip().to_string(), port);
                            eprintln!("Starting media pipeline: wf-recorder {}", args.join(" "));
                            let child = ProcessCommand::new("wf-recorder")
                                .args(&args)
                                .stdin(Stdio::null())
                                .stdout(Stdio::null())
                                .stderr(Stdio::inherit())
                                .spawn()
                                .context("error starting wf-recorder media pipeline")?;
                            media_command = Some(
                                std::iter::once(String::from("wf-recorder"))
                                    .chain(args.iter().cloned())
                                    .collect(),
                            );
                            media = Some(child);
                        }
                    }
                    "PAUSE" => {
                        send_rtsp_response(&mut stream, &cseq, &[("Session", &session_id)], "")?;
                        stop_media(&mut media);
                    }
                    "TEARDOWN" => {
                        send_rtsp_response(&mut stream, &cseq, &[("Session", &session_id)], "")?;
                        teardown_received = true;
                        break;
                    }
                    _ => {
                        send_rtsp_error(&mut stream, &cseq, 405, "Method Not Allowed")?;
                    }
                }
            }
        }
    }

    stop_media(&mut media);

    Ok(MiracastServeResult {
        peer: peer_addr.ip().to_string(),
        presentation_url,
        sink_rtp_port,
        media_started: media_command.is_some(),
        media_command,
        teardown_received,
    })
}

fn send_m3_get_parameters(
    stream: &mut TcpStream,
    outgoing: &mut OutgoingRtspState,
) -> anyhow::Result<()> {
    send_rtsp_request(
        stream,
        outgoing,
        OutgoingRtsp::GetParameters,
        "GET_PARAMETER",
        "rtsp://localhost/wfd1.0",
        &[("Content-Type", "text/parameters")],
        "wfd_client_rtp_ports\r\nwfd_audio_codecs\r\nwfd_video_formats\r\nwfd_display_edid\r\n",
    )
}

fn send_m4_set_parameters(
    stream: &mut TcpStream,
    outgoing: &mut OutgoingRtspState,
    config: &RtspSourceConfig,
    presentation_url: &str,
    sink_rtp_port: Option<u16>,
) -> anyhow::Result<()> {
    let rtp_port = sink_rtp_port.unwrap_or(19000);
    let body = format!(
        "wfd_video_formats: {}\r\n\
         wfd_audio_codecs: {}\r\n\
         wfd_presentation_URL: {presentation_url} none\r\n\
         wfd_client_rtp_ports: RTP/AVP/UDP;unicast {rtp_port} 0 mode=play\r\n",
        config.video_formats,
        if config.audio.is_some() {
            "AAC 0000000F 00"
        } else {
            "none"
        }
    );

    send_rtsp_request(
        stream,
        outgoing,
        OutgoingRtsp::SetParameters,
        "SET_PARAMETER",
        "rtsp://localhost/wfd1.0",
        &[("Content-Type", "text/parameters")],
        &body,
    )
}

fn send_trigger_setup(
    stream: &mut TcpStream,
    outgoing: &mut OutgoingRtspState,
) -> anyhow::Result<()> {
    send_rtsp_request(
        stream,
        outgoing,
        OutgoingRtsp::TriggerSetup,
        "SET_PARAMETER",
        "rtsp://localhost/wfd1.0",
        &[("Content-Type", "text/parameters")],
        "wfd_trigger_method: SETUP\r\n",
    )
}

fn send_rtsp_request(
    stream: &mut TcpStream,
    outgoing: &mut OutgoingRtspState,
    kind: OutgoingRtsp,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> anyhow::Result<()> {
    let cseq = outgoing.next_cseq;
    outgoing.next_cseq += 1;
    outgoing.pending.insert(cseq, kind);

    let mut message = format!("{method} {uri} RTSP/1.0\r\nCSeq: {cseq}\r\n");
    write_headers_and_body(&mut message, headers, body);
    stream.write_all(message.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn send_rtsp_response(
    stream: &mut TcpStream,
    cseq: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> anyhow::Result<()> {
    let mut message = format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n");
    write_headers_and_body(&mut message, headers, body);
    stream.write_all(message.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn send_rtsp_error(
    stream: &mut TcpStream,
    cseq: &str,
    code: u16,
    reason: &str,
) -> anyhow::Result<()> {
    let message = format!("RTSP/1.0 {code} {reason}\r\nCSeq: {cseq}\r\nContent-Length: 0\r\n\r\n");
    stream.write_all(message.as_bytes())?;
    stream.flush()?;
    Ok(())
}

fn write_headers_and_body(message: &mut String, headers: &[(&str, &str)], body: &str) {
    for (name, value) in headers {
        message.push_str(name);
        message.push_str(": ");
        message.push_str(value);
        message.push_str("\r\n");
    }
    message.push_str("Content-Length: ");
    message.push_str(&body.len().to_string());
    message.push_str("\r\n\r\n");
    message.push_str(body);
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

fn read_rtsp_message(reader: &mut BufReader<TcpStream>) -> anyhow::Result<Option<RtspMessage>> {
    let mut start = String::new();
    let n = match reader.read_line(&mut start) {
        Ok(n) => n,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            return Ok(None);
        }
        Err(err) => return Err(err).context("error reading RTSP start line"),
    };
    if n == 0 {
        return Ok(None);
    }

    let start = start.trim_end_matches(['\r', '\n']).to_owned();
    if start.is_empty() {
        return Ok(None);
    }

    let mut headers = BTreeMap::new();
    loop {
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("error reading RTSP header")?;
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }

        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
    }

    let content_length = header(&headers, "content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; content_length];
    if content_length != 0 {
        reader
            .read_exact(&mut body)
            .context("error reading RTSP body")?;
    }
    let body = String::from_utf8(body).context("RTSP body was not UTF-8")?;

    if start.starts_with("RTSP/") {
        let mut parts = start.split_whitespace();
        let _version = parts.next();
        let code = parts
            .next()
            .and_then(|code| code.parse().ok())
            .ok_or_else(|| anyhow!("invalid RTSP response line: {start}"))?;
        Ok(Some(RtspMessage::Response {
            code,
            headers,
            body,
        }))
    } else {
        let mut parts = start.split_whitespace();
        let method = parts
            .next()
            .ok_or_else(|| anyhow!("invalid RTSP request line: {start}"))?
            .to_owned();
        let _uri = parts
            .next()
            .ok_or_else(|| anyhow!("invalid RTSP request line: {start}"))?
            .to_owned();
        Ok(Some(RtspMessage::Request {
            method,
            headers,
            body,
        }))
    }
}

fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a String> {
    headers.get(&name.to_ascii_lowercase())
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

fn parse_rtsp_parameters(body: &str) -> BTreeMap<String, String> {
    body.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() {
                None
            } else if let Some((name, value)) = line.split_once(':') {
                Some((name.trim().to_owned(), value.trim().to_owned()))
            } else {
                Some((line.to_owned(), String::new()))
            }
        })
        .collect()
}

fn parse_wfd_client_rtp_port_from_params(params: &BTreeMap<String, String>) -> Option<u16> {
    params
        .get("wfd_client_rtp_ports")
        .and_then(|value| value.split_whitespace().nth(1))
        .and_then(|port| port.parse().ok())
}

fn parse_transport_client_port(transport: &str) -> Option<u16> {
    transport.split(';').find_map(|part| {
        let part = part.trim();
        let value = part.strip_prefix("client_port=")?;
        value
            .split(['-', ','])
            .next()
            .and_then(|port| port.parse().ok())
    })
}

fn build_get_parameter_response(
    request_body: &str,
    config: &RtspSourceConfig,
    sink_rtp_port: Option<u16>,
    presentation_url: &str,
) -> String {
    let requested = request_body
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    let requested = if requested.is_empty() {
        vec![
            "wfd_video_formats",
            "wfd_audio_codecs",
            "wfd_presentation_URL",
            "wfd_client_rtp_ports",
        ]
    } else {
        requested
    };

    let mut body = String::new();
    for param in requested {
        match param {
            "wfd_video_formats" => {
                body.push_str("wfd_video_formats: ");
                body.push_str(&config.video_formats);
                body.push_str("\r\n");
            }
            "wfd_audio_codecs" => {
                body.push_str("wfd_audio_codecs: ");
                body.push_str(if config.audio.is_some() {
                    "AAC 0000000F 00"
                } else {
                    "none"
                });
                body.push_str("\r\n");
            }
            "wfd_presentation_URL" => {
                body.push_str("wfd_presentation_URL: ");
                body.push_str(presentation_url);
                body.push_str(" none\r\n");
            }
            "wfd_client_rtp_ports" => {
                let port = sink_rtp_port.unwrap_or(19000);
                body.push_str(&format!(
                    "wfd_client_rtp_ports: RTP/AVP/UDP;unicast {port} 0 mode=play\r\n"
                ));
            }
            "wfd_content_protection" => body.push_str("wfd_content_protection: none\r\n"),
            "wfd_display_edid" => body.push_str("wfd_display_edid: none\r\n"),
            "wfd_coupled_sink" => body.push_str("wfd_coupled_sink: none\r\n"),
            _ => {
                body.push_str(param);
                body.push_str(": none\r\n");
            }
        }
    }

    body
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

fn validate_video_formats(video_formats: &str) -> anyhow::Result<()> {
    let fields = video_formats.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 13 {
        bail!(
            "WFD video format string must contain 13 whitespace-separated fields, got {}",
            fields.len()
        );
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

fn build_wf_recorder_args(config: &RtspSourceConfig, host: &str, port: u16) -> Vec<String> {
    let mut args = Vec::new();

    if let Some(output) = &config.output {
        args.extend([String::from("--output"), output.clone()]);
    }

    args.extend([
        String::from("--no-damage"),
        String::from("--muxer"),
        String::from("rtp_mpegts"),
        String::from("--codec"),
        config.codec.clone(),
        String::from("--framerate"),
        config.framerate.to_string(),
        String::from("--pixel-format"),
        String::from("yuv420p"),
        String::from("--codec-param"),
        String::from("preset=ultrafast"),
        String::from("--codec-param"),
        String::from("tune=zerolatency"),
        String::from("--codec-param"),
        format!("b={}k", config.bitrate_kbps),
    ]);

    match &config.audio {
        Some(Some(device)) => {
            args.push(format!("--audio={device}"));
            args.extend([String::from("--audio-codec"), String::from("aac")]);
        }
        Some(None) => {
            args.push(String::from("--audio"));
            args.extend([String::from("--audio-codec"), String::from("aac")]);
        }
        None => (),
    }

    args.extend([
        String::from("-f"),
        format!("rtp://{host}:{port}?pkt_size=1316"),
    ]);

    args
}

fn stop_media(media: &mut Option<Child>) {
    if let Some(mut child) = media.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
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

fn print_serve(result: MiracastServeResult) {
    println!("Miracast RTSP session ended.");
    println!("  Peer: {}", result.peer);
    println!("  Presentation URL: {}", result.presentation_url);
    if let Some(port) = result.sink_rtp_port {
        println!("  Sink RTP port: {port}");
    }
    println!(
        "  Media pipeline: {}",
        if result.media_started {
            "started"
        } else {
            "not started"
        }
    );
    if let Some(command) = &result.media_command {
        println!("  Command: {}", command.join(" "));
    }
    println!(
        "  Teardown: {}",
        if result.teardown_received {
            "received"
        } else {
            "not received"
        }
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_rtsp_config() -> RtspSourceConfig {
        RtspSourceConfig {
            bind: String::from("127.0.0.1"),
            port: 7236,
            accept_timeout: Some(Duration::from_secs(1)),
            session_timeout: Some(Duration::from_secs(1)),
            output: Some(String::from("eDP-1")),
            framerate: 30,
            bitrate_kbps: 6000,
            codec: String::from("libx264"),
            audio: None,
            no_media: true,
            video_formats: String::from(
                "00 00 01 01 00000020 00000000 00000000 00 0000 0000 00 none none",
            ),
        }
    }

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
    fn parses_rtsp_parameters_and_transport_ports() {
        let params = parse_rtsp_parameters(
            "wfd_client_rtp_ports: RTP/AVP/UDP;unicast 19000 0 mode=play\r\n\
             wfd_video_formats\r\n",
        );

        assert_eq!(parse_wfd_client_rtp_port_from_params(&params), Some(19000));
        assert_eq!(
            parse_transport_client_port(
                "RTP/AVP/UDP;unicast;client_port=19000-19001;server_port=0-0"
            ),
            Some(19000)
        );
    }

    #[test]
    fn builds_get_parameter_response() {
        let config = test_rtsp_config();
        let body = build_get_parameter_response(
            "wfd_video_formats\r\nwfd_audio_codecs\r\nwfd_presentation_URL\r\n",
            &config,
            Some(19000),
            "rtsp://192.168.49.1:7236/wfd1.0/streamid=0",
        );

        assert!(body.contains("wfd_video_formats: 00 00 01 01"));
        assert!(body.contains("wfd_audio_codecs: none"));
        assert!(
            body.contains("wfd_presentation_URL: rtsp://192.168.49.1:7236/wfd1.0/streamid=0 none")
        );
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

    #[test]
    fn builds_wf_recorder_rtp_command() {
        let config = test_rtsp_config();
        let args = build_wf_recorder_args(&config, "192.168.49.2", 19000);

        assert!(args.contains(&String::from("--output")));
        assert!(args.contains(&String::from("eDP-1")));
        assert!(args.contains(&String::from("rtp_mpegts")));
        assert!(args.contains(&String::from("libx264")));
        assert!(args.contains(&String::from("b=6000k")));
        assert_eq!(
            args.last().map(String::as_str),
            Some("rtp://192.168.49.2:19000?pkt_size=1316")
        );
    }

    #[test]
    fn validates_video_format_field_count() {
        assert!(validate_video_formats(
            "00 00 01 01 00000020 00000000 00000000 00 0000 0000 00 none none"
        )
        .is_ok());
        assert!(validate_video_formats("00 00 01").is_err());
    }

    #[test]
    fn rtsp_source_completes_no_media_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let server = thread::spawn(move || {
            let (stream, peer_addr) = listener.accept().unwrap();
            let mut config = test_rtsp_config();
            config.port = addr.port();
            config.no_media = true;
            run_rtsp_session(stream, peer_addr, config).unwrap()
        });

        let mut stream = TcpStream::connect(addr).unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());

        let msg = read_rtsp_message(&mut reader).unwrap().unwrap();
        let cseq = expect_request(&msg, "OPTIONS");
        send_rtsp_response(&mut stream, &cseq, &[], "").unwrap();

        let msg = read_rtsp_message(&mut reader).unwrap().unwrap();
        let cseq = expect_request(&msg, "GET_PARAMETER");
        send_rtsp_response(
            &mut stream,
            &cseq,
            &[("Content-Type", "text/parameters")],
            "wfd_client_rtp_ports: RTP/AVP/UDP;unicast 19000 0 mode=play\r\n\
             wfd_video_formats: 00 00 01 01 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
             wfd_audio_codecs: none\r\n",
        )
        .unwrap();

        let msg = read_rtsp_message(&mut reader).unwrap().unwrap();
        let cseq = expect_request(&msg, "SET_PARAMETER");
        send_rtsp_response(&mut stream, &cseq, &[], "").unwrap();

        let msg = read_rtsp_message(&mut reader).unwrap().unwrap();
        let cseq = expect_request(&msg, "SET_PARAMETER");
        send_rtsp_response(&mut stream, &cseq, &[], "").unwrap();

        stream
            .write_all(
                concat!(
                    "SETUP rtsp://localhost/wfd1.0/streamid=0 RTSP/1.0\r\n",
                    "CSeq: 101\r\n",
                    "Transport: RTP/AVP/UDP;unicast;client_port=19000-19001\r\n",
                    "\r\n",
                )
                .as_bytes(),
            )
            .unwrap();
        let msg = read_rtsp_message(&mut reader).unwrap().unwrap();
        expect_response(&msg, 200);

        stream
            .write_all(
                concat!(
                    "PLAY rtsp://localhost/wfd1.0/streamid=0 RTSP/1.0\r\n",
                    "CSeq: 102\r\n",
                    "Session: test\r\n",
                    "\r\n",
                )
                .as_bytes(),
            )
            .unwrap();
        let msg = read_rtsp_message(&mut reader).unwrap().unwrap();
        expect_response(&msg, 200);

        stream
            .write_all(
                concat!(
                    "TEARDOWN rtsp://localhost/wfd1.0/streamid=0 RTSP/1.0\r\n",
                    "CSeq: 103\r\n",
                    "Session: test\r\n",
                    "\r\n",
                )
                .as_bytes(),
            )
            .unwrap();
        let msg = read_rtsp_message(&mut reader).unwrap().unwrap();
        expect_response(&msg, 200);

        let result = server.join().unwrap();
        assert_eq!(result.sink_rtp_port, Some(19000));
        assert!(!result.media_started);
        assert!(result.teardown_received);
    }

    fn expect_request(msg: &RtspMessage, method: &str) -> String {
        let RtspMessage::Request {
            method: actual,
            headers,
            ..
        } = msg
        else {
            panic!("expected request");
        };
        assert_eq!(actual, method);
        header(headers, "cseq").cloned().unwrap()
    }

    fn expect_response(msg: &RtspMessage, code: u16) {
        let RtspMessage::Response { code: actual, .. } = msg else {
            panic!("expected response");
        };
        assert_eq!(*actual, code);
    }
}
