//! Animated frame sources.
//!
//! Rust owns the clock: an animation is asked to draw one frame for an elapsed
//! duration, and may report that it is unchanged, finished, or failed.
//!
//! Adding an animation means creating `animation/<name>.rs` with an `Options`
//! type (serde + `clap::Args`) and a `<Name>::new(Options)` constructor, then
//! adding one line to [`animations!`]. That line is the serde tag, the clap
//! subcommand, the factory and the display name at once.

use std::io;
use std::time::Duration;

use clap::Subcommand;
use serde::{Deserialize, Serialize};

use crate::canvas::Canvas;

/// What an animation produced for the current tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// New pixels were drawn and should be sent to the panel.
    New,
    /// Nothing changed; skip the USB transfer.
    Unchanged,
    /// The animation finished on its own.
    Ended,
}

/// A drawable animation.
pub trait Animation {
    /// Advance by `dt` and draw into `canvas`.
    ///
    /// # Errors
    /// Propagate a failure to stop playback.
    fn draw(&mut self, canvas: &mut Canvas, dt: Duration) -> io::Result<Status>;

    /// Optional live input from an external producer.
    ///
    /// # Errors
    /// Propagate a failure to stop playback.
    fn input(&mut self, _input: &[u8]) -> io::Result<()> {
        Ok(())
    }
}

macro_rules! animations {
    ($(
        $(#[$meta:meta])*
        $module:ident => $variant:ident
    ),+ $(,)?) => {
        $(
            pub mod $module;
        )+

        /// The closed set of built-in animations.
        ///
        /// Each variant is a serde tag, a clap subcommand and a factory entry.
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Subcommand)]
        #[serde(rename_all = "snake_case")]
        #[command(rename_all = "snake_case")]
        pub enum AnimationKind {
            $(
                $(#[$meta])*
                $variant($module::Options),
            )+
        }

        impl AnimationKind {
            pub fn create(&self) -> Box<dyn Animation> {
                match self {
                    $(
                        Self::$variant(options) => {
                            Box::new($module::$variant::new(*options))
                        }
                    )+
                }
            }

            pub const fn name(&self) -> &'static str {
                match self {
                    $(Self::$variant(_) => stringify!($module),)+
                }
            }
        }
    };
}

animations! {
    /// Warp-speed starfield
    starfield => Starfield,
}

impl Default for AnimationKind {
    fn default() -> Self {
        Self::Starfield(starfield::Options::default())
    }
}

#[cfg(test)]
mod tests {
    use clap::{Command, FromArgMatches as _};

    use super::*;
    use crate::protocol::SpeedMultiplier;

    fn parse(args: &[&str]) -> AnimationKind {
        let command = AnimationKind::augment_subcommands(Command::new("anim"));
        let matches = command.try_get_matches_from(args).unwrap();
        AnimationKind::from_arg_matches(&matches).unwrap()
    }

    #[test]
    fn animations_define_their_own_cli() {
        let kind = parse(&[
            "anim",
            "starfield",
            "--stars",
            "10",
            "--speed",
            "1.5",
            "--trail",
            "20",
        ]);

        assert_eq!(
            kind,
            AnimationKind::Starfield(starfield::Options {
                stars: 10,
                speed: SpeedMultiplier::new(1.5).unwrap(),
                trail: 20,
            })
        );
        assert_eq!(kind.name(), "starfield");
    }

    #[test]
    fn animation_options_default_from_the_cli() {
        assert_eq!(parse(&["anim", "starfield"]), AnimationKind::default());
    }
}
