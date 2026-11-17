mod anim;
mod commands;
mod usb;

use std::io;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "zenvision",
    version,
    about = "ZenVision OLED Display Controller"
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
}

fn main() -> io::Result<()> {
    let cli = Cli::parse();
    let mut dev = usb::open_device()?;

    match cli.command {
        Command::On => commands::power_on(&mut dev)?,
        Command::Off => commands::power_off(&mut dev)?,
        Command::State => println!("{}", commands::get_engine_state(&mut dev)?),
        Command::Theme { value } => commands::set_theme(&mut dev, value)?,
        Command::Speed { value } => commands::set_speed(&mut dev, value)?,
        Command::Clock { value } => commands::set_clock(&mut dev, value)?,
        Command::Time { value } => commands::set_time(&mut dev, value == "24")?,
        Command::Mode { value } => commands::set_content_mode(&mut dev, value)?,
        Command::Brightness { value } => commands::set_brightness(&mut dev, value)?,
        Command::Boot { value } => commands::set_boot_animation(&mut dev, value == "on")?,
        Command::Sweep { value } => commands::set_screen_sweep(&mut dev, value == "on")?,
        Command::Battery { value } => commands::set_battery(&mut dev, value == "on")?,
        Command::Demo { fps } => anim::run(&mut dev, fps)?,
    }

    Ok(())
}
