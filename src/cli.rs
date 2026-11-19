//! Command-line interface. Clap lives here and in each animation's options.

use std::io::{self, Read, Write};
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::animation::AnimationKind;
use crate::app;
use crate::canvas::{Canvas, GRAYSCALE_BYTES};
use crate::ipc::{self, Reply};
use crate::panel::{Panel, RealPanel, TerminalPanel};
use crate::preview;
use crate::protocol::{ClockStyle, ContentMode, Fps, PanelCommand, Speed, Theme, TimeFormat};
use crate::render::Player;
use crate::state::State;

#[derive(Parser)]
#[command(
    name = "zenvision",
    version,
    about = "ZenVision lid display driver for ASUS Zenbook 14X Space Edition",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Turn panel on
    On,
    /// Turn panel off
    Off,
    /// Set display theme <1-4>
    Theme {
        #[arg(value_parser = bounded::<Theme>)]
        value: Theme,
    },
    /// Set built-in animation speed <1-3>
    Speed {
        #[arg(value_parser = bounded::<Speed>)]
        value: Speed,
    },
    /// Set clock style <1-2>
    Clock {
        #[arg(value_parser = bounded::<ClockStyle>)]
        value: ClockStyle,
    },
    /// Set time format <12|24>
    Time { value: TimeArg },
    /// Set content mode <1-3>
    Mode {
        #[arg(value_parser = bounded::<ContentMode>)]
        value: ContentMode,
    },
    /// Set display brightness <0-255>
    Brightness { value: u8 },
    /// Set boot animation <on|off>
    Boot { value: OnOff },
    /// Set screen sweep <on|off>
    Sweep { value: OnOff },
    /// Set battery indicator <on|off>
    Battery { value: OnOff },
    /// Display current panel state
    State,
    /// Play a custom animation
    Anim(AnimArgs),
    /// Mirror the panel in the terminal (local preview without a daemon)
    Preview(PreviewArgs),
    /// Stream raw 256x64 grayscale frames from stdin to the daemon
    Stream(StreamArgs),
    /// Start the background process
    Daemon,
}

#[derive(Args)]
struct AnimArgs {
    /// Native animation and its options
    #[command(subcommand)]
    animation: Option<AnimationKind>,
    /// Frames per second
    #[arg(long, global = true, value_parser = Fps::from_str)]
    fps: Option<Fps>,
}

#[derive(Args)]
struct PreviewArgs {
    /// Native animation and its options
    #[command(subcommand)]
    animation: Option<AnimationKind>,
    /// Frames per second
    #[arg(long, global = true, value_parser = Fps::from_str)]
    fps: Option<Fps>,
    /// Write raw grayscale frames to stdout for another viewer
    #[arg(long)]
    raw: bool,
}

#[derive(Args)]
struct StreamArgs {
    /// Display frames at this rate (defaults to the daemon's current rate)
    #[arg(long, value_parser = Fps::from_str)]
    fps: Option<Fps>,
}

/// Parse a whole number and hand it to a validated panel type.
fn bounded<T>(value: &str) -> Result<T, String>
where
    T: TryFrom<u8, Error = String>,
{
    value
        .parse::<u8>()
        .map_err(|_| format!("expected a whole number, got {value}"))
        .and_then(T::try_from)
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum TimeArg {
    #[value(name = "12")]
    H12,
    #[value(name = "24")]
    H24,
}

impl From<TimeArg> for TimeFormat {
    fn from(value: TimeArg) -> Self {
        match value {
            TimeArg::H12 => Self::H12,
            TimeArg::H24 => Self::H24,
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum OnOff {
    On,
    Off,
}

impl OnOff {
    const fn is_on(self) -> bool {
        matches!(self, Self::On)
    }
}

/// Parse the CLI, then run the selected command.
///
/// # Errors
/// Propagates panel, socket and IPC failures.
pub fn run() -> io::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Daemon => app::run(),
        Command::Stream(args) => stream(args),
        Command::Preview(args) => preview(args),
        Command::Anim(args) => anim(args),
        command => one_shot(&command),
    }
}

fn one_shot(command: &Command) -> io::Result<()> {
    let Some(command) = panel_command(command) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a panel command",
        ));
    };

    let paths = ipc::Paths::new()?;
    if let Some(reply) = ipc::send(&paths, ipc::Command::Panel(command.clone()))? {
        return handle_reply(reply);
    }

    let mut panel = RealPanel::open()?;
    if let Some(output) = panel.apply(&command)? {
        if !output.is_empty() {
            println!("{output}");
        }
    }
    Ok(())
}

fn panel_command(command: &Command) -> Option<PanelCommand> {
    Some(match command {
        Command::On => PanelCommand::On,
        Command::Off => PanelCommand::Off,
        Command::Theme { value } => PanelCommand::Theme { value: *value },
        Command::Speed { value } => PanelCommand::Speed { value: *value },
        Command::Clock { value } => PanelCommand::Clock { value: *value },
        Command::Time { value } => PanelCommand::Time {
            format: (*value).into(),
        },
        Command::Mode { value } => PanelCommand::Mode { value: *value },
        Command::Brightness { value } => PanelCommand::Brightness { value: *value },
        Command::Boot { value } => PanelCommand::Boot {
            value: value.is_on(),
        },
        Command::Sweep { value } => PanelCommand::Sweep {
            value: value.is_on(),
        },
        Command::Battery { value } => PanelCommand::Battery {
            value: value.is_on(),
        },
        Command::State => PanelCommand::State,
        Command::Anim(_) | Command::Preview(_) | Command::Stream(_) | Command::Daemon => {
            return None;
        }
    })
}

fn handle_reply(reply: Reply) -> io::Result<()> {
    match reply {
        Reply::Ok { message } => {
            if let Some(message) = message {
                if !message.is_empty() {
                    println!("{message}");
                }
            }
            Ok(())
        }
        Reply::Error { message } => Err(io::Error::other(message)),
    }
}

/// The persisted animation, for the no-daemon path.
fn local_animation(animation: Option<AnimationKind>, fps: Option<Fps>) -> (AnimationKind, Fps) {
    let state = State::load();
    (
        animation.unwrap_or(state.animation),
        fps.unwrap_or(state.fps),
    )
}

fn anim(args: AnimArgs) -> io::Result<()> {
    let paths = ipc::Paths::new()?;

    // Send only what the user actually asked for. `None` keeps the daemon's
    // current animation options or frame rate.
    let request = ipc::Command::Play {
        animation: args.animation.clone(),
        fps: args.fps,
    };
    if let Some(reply) = ipc::send(&paths, request)? {
        return handle_reply(reply);
    }

    // No daemon: drive the panel directly with the same player.
    let (kind, fps) = local_animation(args.animation, args.fps);
    let mut panel = RealPanel::open()?;
    let player = Player::new(kind, fps);
    app::run_animation(&mut panel, player)
}

fn preview(args: PreviewArgs) -> io::Result<()> {
    let paths = ipc::Paths::new()?;

    // With a daemon running, show exactly the frames the panel receives
    // (including animation changes and pauses for theme/clock).
    if let Some(stream) = ipc::connect_preview(&paths)? {
        if args.raw {
            return preview::follow_raw(stream);
        }
        let mut panel = TerminalPanel::new();
        return preview::follow(stream, &mut panel);
    }

    // No daemon: render the requested animation locally.
    let (kind, fps) = local_animation(args.animation, args.fps);
    let player = Player::new(kind, fps);
    if args.raw {
        let mut panel = RawPanel::new();
        return app::run_animation(&mut panel, player);
    }
    let mut panel = TerminalPanel::new();
    app::run_animation(&mut panel, player)
}

fn stream(args: StreamArgs) -> io::Result<()> {
    let paths = ipc::Paths::new()?;
    let mut socket = ipc::connect_frames(&paths).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("could not reach the daemon frame socket: {error}"),
        )
    })?;

    // The daemon displays at its own rate and only then accepts the next
    // message, so these writes block instead of building a queue. A running
    // daemon also resumes host frames and optionally adopts the new rate.
    let request = ipc::Command::Play {
        animation: None,
        fps: args.fps,
    };
    if let Some(reply) = ipc::send(&paths, request)? {
        handle_reply(reply)?;
    }

    let mut stdin = io::stdin().lock();
    let mut frame = vec![0u8; GRAYSCALE_BYTES];
    let mut message = vec![0u8; GRAYSCALE_BYTES + 1];
    message[0] = ipc::TAG_FRAME;

    loop {
        match stdin.read_exact(&mut frame) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }

        message[1..].copy_from_slice(&frame);
        socket.write_all(&message)?;
    }
}

/// Raw output for external viewers such as `ffplay` or `mpv`.
struct RawPanel {
    out: io::Stdout,
}

impl RawPanel {
    fn new() -> Self {
        Self { out: io::stdout() }
    }
}

impl Panel for RawPanel {
    fn apply(&mut self, _command: &PanelCommand) -> io::Result<Option<String>> {
        Ok(None)
    }

    fn frame(&mut self, canvas: &Canvas) -> io::Result<()> {
        self.out.write_all(canvas.pixels())?;
        self.out.flush()
    }
}
