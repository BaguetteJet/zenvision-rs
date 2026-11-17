mod anim;
mod commands;
mod daemon;
mod usb;

use std::io;

use clap::{Parser, Subcommand};

use crate::usb::Dev;

#[derive(Parser)]
#[command(
    name = "zenvision",
    version,
    about = "ZenVision lid display driver for ASUS Zenbook 14X Space Edition"
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
pub(crate) enum Command {
    /// Turn panel on
    On,
    /// Turn panel off
    Off,
    /// Set display theme <1-4>
    Theme {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=4))]
        value: u8,
    },
    /// Set animation speed <1-3>
    Speed {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=3))]
        value: u8,
    },
    /// Set clock style <1-2>
    Clock {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=2))]
        value: u8,
    },
    /// Set time format <12|24>
    Time {
        #[arg(value_parser = ["12", "24"])]
        value: String,
    },
    /// Set content mode <1-3>
    Mode {
        #[arg(value_parser = clap::value_parser!(u8).range(1..=3))]
        value: u8,
    },
    /// Set display brightness <0-255>
    Brightness { value: u8 },
    /// Set boot animation <on|off>
    Boot {
        #[arg(value_parser = ["on", "off"])]
        value: String,
    },
    /// Set screen sweep <on|off>
    Sweep {
        #[arg(value_parser = ["on", "off"])]
        value: String,
    },
    /// Set battery indicator <on|off>
    Battery {
        #[arg(value_parser = ["on", "off"])]
        value: String,
    },
    /// Display current panel state
    State,
    /// Play an animation
    Demo {
        /// Frames per second
        #[arg(
            long,
            default_value_t = 20,
            value_parser = clap::value_parser!(u32).range(1..=60)
        )]
        fps: u32,
    },
    /// Run in the background and accept commands
    Daemon,
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cli = Cli::parse_from(&args);

    if matches!(&cli.command, Command::Daemon) {
        return daemon::run();
    }

    // If a daemon owns the panel, hand the command over instead of fighting it.
    if let Some(reply) = daemon::send(&args[1..])? {
        if let Some(error) = reply.strip_prefix("error: ") {
            return Err(io::Error::other(error.to_string()));
        }
        if !reply.is_empty() && reply != "ok" {
            println!("{reply}");
        }
        return Ok(());
    }

    let mut dev = usb::open_device()?;
    match &cli.command {
        Command::Demo { fps } => anim::run(&mut dev, *fps),
        Command::Daemon => unreachable!(),
        command => {
            if let Some(out) = execute(&mut dev, command)? {
                println!("{out}");
            }
            Ok(())
        }
    }
}

// Apply one panel command; returns text only for commands that report state.
pub(crate) fn execute(dev: &mut Dev, cmd: &Command) -> io::Result<Option<String>> {
    match cmd {
        Command::On => commands::power_on(dev)?,
        Command::Off => commands::power_off(dev)?,
        Command::State => return Ok(Some(commands::get_engine_state(dev)?)),
        Command::Theme { value } => commands::set_theme(dev, *value)?,
        Command::Speed { value } => commands::set_speed(dev, *value)?,
        Command::Clock { value } => commands::set_clock(dev, *value)?,
        Command::Time { value } => commands::set_time(dev, value.as_str() == "24")?,
        Command::Mode { value } => commands::set_content_mode(dev, *value)?,
        Command::Brightness { value } => commands::set_brightness(dev, *value)?,
        Command::Boot { value } => commands::set_boot_animation(dev, value.as_str() == "on")?,
        Command::Sweep { value } => commands::set_screen_sweep(dev, value.as_str() == "on")?,
        Command::Battery { value } => commands::set_battery(dev, value.as_str() == "on")?,
        Command::Demo { .. } | Command::Daemon => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "not a panel command",
            ));
        }
    }
    Ok(None)
}
