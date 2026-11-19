//! Animated frame sources.
//!
//! Rust owns the clock: an animation is asked to draw one frame for an elapsed
//! duration, and may report that it is unchanged, finished, or failed.

use std::io;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::canvas::Canvas;

pub mod starfield;

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

/// The closed set of built-in animations.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnimationKind {
    Starfield(starfield::Options),
}

impl AnimationKind {
    pub fn create(&self) -> Box<dyn Animation> {
        match self {
            Self::Starfield(options) => Box::new(starfield::Starfield::new(*options)),
        }
    }

    pub const fn name(&self) -> &'static str {
        match self {
            Self::Starfield(_) => "starfield",
        }
    }
}

impl Default for AnimationKind {
    fn default() -> Self {
        Self::Starfield(starfield::Options::default())
    }
}
