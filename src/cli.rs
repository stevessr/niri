use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use clap_complete::Shell;
use niri_ipc::{Action, OutputAction};

use crate::utils::version;

#[derive(Parser)]
#[command(author, version = version(), about, long_about = None)]
#[command(args_conflicts_with_subcommands = true)]
#[command(subcommand_value_name = "SUBCOMMAND")]
#[command(subcommand_help_heading = "Subcommands")]
pub struct Cli {
    /// Path to config file (default: `$XDG_CONFIG_HOME/niri/config.kdl`).
    ///
    /// This can also be set with the `NIRI_CONFIG` environment variable. If both are set, the
    /// command line argument takes precedence.
    #[arg(short, long)]
    pub config: Option<PathBuf>,
    /// Import environment globally to systemd and D-Bus, run D-Bus services.
    ///
    /// Set this flag in a systemd service started by your display manager, or when running
    /// manually as your main compositor instance. Do not set when running as a nested window, or
    /// on a TTY as your non-main compositor instance, to avoid messing up the global environment.
    #[arg(long)]
    pub session: bool,
    /// Command to run upon compositor startup.
    #[arg(last = true)]
    pub command: Vec<OsString>,

    #[command(subcommand)]
    pub subcommand: Option<Sub>,
}

#[derive(Subcommand)]
pub enum Sub {
    /// Communicate with the running niri instance.
    Msg {
        #[command(subcommand)]
        msg: Msg,
        /// Format output as JSON.
        #[arg(short, long)]
        json: bool,
    },
    /// Validate the config file.
    Validate {
        /// Path to config file (default: `$XDG_CONFIG_HOME/niri/config.kdl`).
        ///
        /// This can also be set with the `NIRI_CONFIG` environment variable. If both are set, the
        /// command line argument takes precedence.
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
    /// Experimental Miracast / Wi-Fi Display helper commands.
    Miracast {
        #[command(subcommand)]
        command: MiracastCommand,
    },
    /// Cause a panic to check if the backtraces are good.
    Panic,
    /// Generate shell completions.
    Completions { shell: CompletionShell },
}

#[derive(Subcommand)]
pub enum MiracastCommand {
    /// Scan for Wi-Fi Direct peers, including Miracast sinks.
    Scan {
        /// Wi-Fi interface controlled by wpa_supplicant.
        #[arg(short = 'i', long)]
        ifname: Option<String>,
        /// How long to scan for, in seconds.
        #[arg(short, long, default_value_t = 8)]
        timeout: u64,
        /// Print machine-readable JSON.
        #[arg(short, long)]
        json: bool,
        /// Show only peers that advertise Wi-Fi Display / Miracast capabilities.
        #[arg(long)]
        miracast_only: bool,
        /// Flush wpa_supplicant's P2P peer cache before scanning.
        #[arg(long)]
        flush: bool,
        /// Do not advertise local Wi-Fi Display source capabilities before scanning.
        #[arg(long)]
        no_wfd: bool,
        /// Wi-Fi Display Device Information subelement payload passed to wpa_cli.
        #[arg(long, default_value = "000600111c440032")]
        wfd_device_info: String,
    },
    /// Establish a Wi-Fi Direct link to a discovered Miracast sink.
    Connect {
        /// Peer P2P device address from `niri miracast scan`.
        #[arg()]
        peer: String,
        /// Wi-Fi interface controlled by wpa_supplicant.
        #[arg(short = 'i', long)]
        ifname: Option<String>,
        /// How long to wait for command completion, in seconds.
        #[arg(short, long, default_value_t = 45)]
        timeout: u64,
        /// Print machine-readable JSON.
        #[arg(short, long)]
        json: bool,
        /// Use this PIN with the peer's keypad method instead of PBC.
        #[arg(long, conflicts_with = "display_pin")]
        pin: Option<String>,
        /// Ask wpa_supplicant to generate and display a PIN instead of PBC.
        #[arg(long)]
        display_pin: bool,
        /// Request a persistent P2P group.
        #[arg(long)]
        persistent: bool,
        /// Force join-client behavior for an already-running Group Owner.
        #[arg(long)]
        join: bool,
        /// Request a Provision Discovery exchange before group formation.
        #[arg(long)]
        provdisc: bool,
        /// Do not let wpa_supplicant automatically decide whether to join an existing GO.
        #[arg(long)]
        no_auto: bool,
        /// Override P2P GO intent, 0..15.
        #[arg(long)]
        go_intent: Option<u8>,
        /// Force an operating frequency in MHz.
        #[arg(long)]
        freq: Option<u32>,
        /// Do not advertise local Wi-Fi Display source capabilities before connecting.
        #[arg(long)]
        no_wfd: bool,
        /// Wi-Fi Display Device Information subelement payload passed to wpa_cli.
        #[arg(long, default_value = "000600111c440032")]
        wfd_device_info: String,
    },
    /// Remove a P2P group interface.
    Disconnect {
        /// P2P group interface to remove, for example `p2p-wlan0-0`.
        #[arg()]
        group_ifname: String,
        /// Wi-Fi interface controlled by wpa_supplicant.
        #[arg(short = 'i', long)]
        ifname: Option<String>,
        /// Print machine-readable JSON.
        #[arg(short, long)]
        json: bool,
    },
    /// Run a Wi-Fi Display RTSP source and stream the current Wayland output.
    Serve {
        /// Address to listen on for the sink RTSP connection.
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        /// RTSP port to listen on.
        #[arg(long, default_value_t = 7236)]
        port: u16,
        /// Stop waiting for the sink after this many seconds; 0 means forever.
        #[arg(long, default_value_t = 0)]
        accept_timeout: u64,
        /// Stop a connected RTSP session after this many idle seconds; 0 means forever.
        #[arg(long, default_value_t = 0)]
        session_timeout: u64,
        /// niri output name to capture with wf-recorder.
        #[arg(short, long)]
        output: Option<String>,
        /// Video framerate to request from wf-recorder.
        #[arg(short, long, default_value_t = 30)]
        framerate: u32,
        /// Video bitrate to request from wf-recorder, in kbit/s.
        #[arg(long, default_value_t = 8000)]
        bitrate_kbps: u32,
        /// Video codec to request from wf-recorder.
        #[arg(long, default_value = "libx264")]
        codec: String,
        /// Include audio using wf-recorder's default audio source.
        #[arg(long)]
        audio: bool,
        /// Include audio from the specified PipeWire/Pulse source.
        #[arg(long, conflicts_with = "audio")]
        audio_device: Option<String>,
        /// Do the RTSP handshake but do not launch wf-recorder.
        #[arg(long)]
        no_media: bool,
        /// Override the selected WFD H.264 video format string.
        #[arg(
            long,
            default_value = "00 00 01 01 00000020 00000000 00000000 00 0000 0000 00 none none"
        )]
        video_formats: String,
        /// Print machine-readable JSON after the session ends.
        #[arg(short, long)]
        json: bool,
    },
}

#[derive(Subcommand)]
pub enum Msg {
    /// List connected outputs.
    Outputs,
    /// List workspaces.
    Workspaces,
    /// List open windows.
    Windows,
    /// List open layer-shell surfaces.
    Layers,
    /// Get the configured keyboard layouts.
    KeyboardLayouts,
    /// Print information about the focused output.
    FocusedOutput,
    /// Print information about the focused window.
    FocusedWindow,
    /// Pick a window with the mouse and print information about it.
    PickWindow,
    /// Pick a color from the screen with the mouse.
    PickColor,
    /// Perform an action.
    Action {
        #[command(subcommand)]
        action: Action,
    },
    /// Change output configuration temporarily.
    ///
    /// The configuration is changed temporarily and not saved into the config file. If the output
    /// configuration subsequently changes in the config file, these temporary changes will be
    /// forgotten.
    Output {
        /// Output name.
        ///
        /// Run `niri msg outputs` to see the output names.
        #[arg()]
        output: String,
        /// Configuration to apply.
        #[command(subcommand)]
        action: OutputAction,
    },
    /// Start continuously receiving events from the compositor.
    EventStream,
    /// Print the version of the running niri instance.
    Version,
    /// Request an error from the running niri instance.
    RequestError,
    /// Print the overview state.
    OverviewState,
    /// List screencasts.
    Casts,
}

#[derive(Clone, Debug, clap::ValueEnum)]
pub enum CompletionShell {
    Bash,
    Elvish,
    Fish,
    PowerShell,
    Zsh,
    Nushell,
}

impl TryFrom<CompletionShell> for Shell {
    type Error = &'static str;

    fn try_from(shell: CompletionShell) -> Result<Self, Self::Error> {
        match shell {
            CompletionShell::Bash => Ok(Shell::Bash),
            CompletionShell::Elvish => Ok(Shell::Elvish),
            CompletionShell::Fish => Ok(Shell::Fish),
            CompletionShell::PowerShell => Ok(Shell::PowerShell),
            CompletionShell::Zsh => Ok(Shell::Zsh),
            CompletionShell::Nushell => Err("Nushell should be handled separately"),
        }
    }
}
