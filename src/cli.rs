//! Command-line interface. Clap lives here and in each animation's options.

use std::io::{self, Read, Write};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::animation::AnimationKind;
use crate::app;
use crate::canvas::GRAYSCALE_BYTES;
use crate::ipc::{self, Reply};
use crate::panel::{Panel, RealPanel, TerminalPanel};
use crate::preview;
use crate::protocol::{ClockStyle, ContentMode, Fps, PanelCommand, Speed, Theme, TimeFormat};
use crate::render::Player;

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
        #[arg(value_parser = Theme::from_str)]
        value: Theme,
    },
    /// Set built-in animation speed <1-3>
    Speed {
        #[arg(value_parser = Speed::from_str)]
        value: Speed,
    },
    /// Set clock style <1-2>
    Clock {
        #[arg(value_parser = ClockStyle::from_str)]
        value: ClockStyle,
    },
    /// Set time format <12|24>
    Time { value: TimeArg },
    /// Set content mode <1-3>
    Mode {
        #[arg(value_parser = ContentMode::from_str)]
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
    Preview(AnimArgs),
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
struct StreamArgs {
    /// Pace the producer at this rate
    #[arg(long, default_value = "30", value_parser = Fps::from_str)]
    fps: Fps,
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
    if let Some(output) = panel.apply(&command)?
        && !output.is_empty()
    {
        println!("{output}");
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
            if let Some(message) = message
                && !message.is_empty()
            {
                println!("{message}");
            }
            Ok(())
        }
        Reply::Error { message } => Err(io::Error::other(message)),
    }
}

fn animation(args: AnimArgs) -> (AnimationKind, Fps) {
    (
        args.animation.unwrap_or_default(),
        args.fps.unwrap_or_default(),
    )
}

fn anim(args: AnimArgs) -> io::Result<()> {
    let (kind, fps) = animation(args);
    let paths = ipc::Paths::new()?;

    let request = ipc::Command::Play {
        animation: Some(kind.clone()),
        fps: Some(fps),
    };
    if let Some(reply) = ipc::send(&paths, request)? {
        return handle_reply(reply);
    }

    // No daemon: drive the panel directly with the same player.
    let mut panel = RealPanel::open()?;
    let player = Player::new(kind, fps);
    app::run_animation(&mut panel, player)
}

fn preview(args: AnimArgs) -> io::Result<()> {
    let paths = ipc::Paths::new()?;

    // With a daemon running, show exactly the frames the panel receives
    // (including animation changes and pauses for theme/clock).
    if let Some(stream) = ipc::connect_preview(&paths)? {
        let mut panel = TerminalPanel::new();
        return preview::follow(stream, &mut panel);
    }

    // No daemon: render the requested animation locally.
    let (kind, fps) = animation(args);
    let mut panel = TerminalPanel::new();
    let player = Player::new(kind, fps);
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

    let period = Duration::from_secs_f64(1.0 / f64::from(args.fps.get()));
    let mut stdin = io::stdin().lock();
    let mut frame = vec![0u8; GRAYSCALE_BYTES];
    let mut next = Instant::now();

    loop {
        match stdin.read_exact(&mut frame) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(error) => return Err(error),
        }

        socket.write_all(&frame)?;

        next += period;
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        } else {
            next = now;
        }
    }
}
