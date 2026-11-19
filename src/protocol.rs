//! Panel protocol: validated command types and their USB payloads.
//!
//! This module is pure. It never touches the device, and the wall clock only
//! enters through [`Time::now`] so callers can pass a snapshot in for tests.

use std::fmt;

use chrono::{Datelike, Local, Timelike};
use serde::{Deserialize, Serialize};

/// Maximum command payload accepted by the panel.
pub const MAX_PAYLOAD: usize = 512;

/// Query the built-in engine state (`f1 03`).
pub const STATE_QUERY: &[u8] = &[0xf1, 0x03];

/// Built-in content mode that shows the clock. Used when releasing the panel.
pub const BUILTIN_MODE: PanelCommand = PanelCommand::Mode {
    value: ContentMode::One,
};

/// One command understood by the panel firmware.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PanelCommand {
    /// Power the panel on and restore the default theme.
    On,
    /// Power the panel off.
    Off,
    /// Set the built-in theme (1-4).
    Theme { value: Theme },
    /// Set the built-in animation speed (1-3).
    Speed { value: Speed },
    /// Set the built-in clock style (1-2).
    Clock { value: ClockStyle },
    /// Set the clock time format.
    Time { format: TimeFormat },
    /// Set the content mode (1-3; 2 is host frames).
    Mode { value: ContentMode },
    /// Set panel brightness (0-255).
    Brightness { value: u8 },
    /// Enable or disable the boot animation.
    Boot { value: bool },
    /// Enable or disable the screen sweep effect.
    Sweep { value: bool },
    /// Show or hide the battery indicator.
    Battery { value: bool },
    /// Report the built-in engine state.
    State,
}

impl PanelCommand {
    /// Encode this command into zero or more USB payloads.
    ///
    /// [`PanelCommand::State`] is a query and produces no writes; use
    /// [`STATE_QUERY`] for it instead.
    pub fn payloads(&self, now: Time) -> Vec<Vec<u8>> {
        match *self {
            Self::On => vec![
                vec![0x30, 0x05, 0x04, 0x00, 0x00, 0x00, 0x03],
                vec![0x30, 0x05, 0x02, 0x00, 0x04],
            ],
            Self::Off => vec![
                vec![0x30, 0x05, 0x04, 0x00, 0x00, 0x00, 0x00],
                vec![0x31, 0x02, 0x00, 0x04],
            ],
            Self::Theme { value } => {
                vec![vec![0x30, 0x05, 0x02, 0x00, value.value()]]
            }
            Self::Speed { value } => vec![vec![0x33, 0x01, value.value()]],
            Self::Clock { value } => vec![vec![0x30, 0x05, 0x01, value.value()]],
            Self::Time { format } => {
                let mut payload = vec![0x40, 0x09];
                payload.extend_from_slice(&now.year.to_le_bytes());
                payload.extend_from_slice(&[
                    now.month,
                    now.day,
                    now.hour,
                    now.minute,
                    now.second,
                    u8::from(format == TimeFormat::H24),
                    now.weekday,
                ]);
                vec![payload]
            }
            Self::Mode { value } => vec![vec![
                0x30,
                0x06,
                0x05,
                0x00,
                0x00,
                0x00,
                0x00,
                value.value(),
            ]],
            Self::Brightness { value } => vec![vec![0x35, 0x01, value]],
            Self::Boot { value } => {
                let value = if value { 0x02 } else { 0x00 };
                vec![vec![0x32, 0x02, value, value]]
            }
            Self::Sweep { value } => {
                let (a, b) = if value { (0x02, 0x03) } else { (0x00, 0x04) };
                vec![vec![0x31, 0x02, a, b]]
            }
            Self::Battery { value } => {
                let value = if value { 0x03 } else { 0x01 };
                vec![vec![0x30, 0x05, 0x04, 0x00, 0x00, 0x00, value]]
            }
            Self::State => Vec::new(),
        }
    }
}

/// A validated theme value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    One,
    Two,
    Three,
    Four,
}

impl Theme {
    pub const fn value(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 3,
            Self::Four => 4,
        }
    }

    pub const fn from_value(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            3 => Some(Self::Three),
            4 => Some(Self::Four),
            _ => None,
        }
    }
}

/// A validated built-in animation speed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speed {
    Slow,
    Normal,
    Fast,
}

impl Speed {
    pub const fn value(self) -> u8 {
        match self {
            Self::Slow => 1,
            Self::Normal => 2,
            Self::Fast => 3,
        }
    }

    pub const fn from_value(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Slow),
            2 => Some(Self::Normal),
            3 => Some(Self::Fast),
            _ => None,
        }
    }
}

/// A validated clock style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClockStyle {
    One,
    Two,
}

impl ClockStyle {
    pub const fn value(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }

    pub const fn from_value(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            _ => None,
        }
    }
}

/// A validated content mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentMode {
    One,
    Two,
    Three,
}

impl ContentMode {
    pub const fn value(self) -> u8 {
        match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Three => 3,
        }
    }

    pub const fn from_value(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::One),
            2 => Some(Self::Two),
            3 => Some(Self::Three),
            _ => None,
        }
    }
}

/// Clock time format.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeFormat {
    #[serde(rename = "12")]
    H12,
    #[serde(rename = "24")]
    #[default]
    H24,
}

/// A validated frame rate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct Fps(u32);

impl Fps {
    pub const DEFAULT: Self = Self(30);
    pub const MAX: u32 = 60;

    /// Validate a frame rate.
    ///
    /// # Errors
    /// Returns a message when `value` is outside `1..=60`.
    pub fn new(value: u32) -> Result<Self, String> {
        if (1..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(format!("fps must be between 1 and {}", Self::MAX))
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for Fps {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u32> for Fps {
    type Error = String;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Fps> for u32 {
    fn from(value: Fps) -> Self {
        value.0
    }
}

impl fmt::Display for Fps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A finite, non-negative animation speed multiplier.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f32", into = "f32")]
pub struct SpeedMultiplier(f32);

impl SpeedMultiplier {
    pub const DEFAULT: Self = Self(1.0);
    pub const MAX: f32 = 100.0;

    /// Validate a speed multiplier.
    ///
    /// # Errors
    /// Returns a message for NaN, infinities and values outside `0..=100`.
    pub fn new(value: f32) -> Result<Self, String> {
        if value.is_finite() && (0.0..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(format!(
                "speed must be a finite number between 0 and {}",
                Self::MAX
            ))
        }
    }

    pub const fn get(self) -> f32 {
        self.0
    }
}

impl Default for SpeedMultiplier {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<f32> for SpeedMultiplier {
    type Error = String;

    fn try_from(value: f32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<SpeedMultiplier> for f32 {
    fn from(value: SpeedMultiplier) -> Self {
        value.0
    }
}

impl fmt::Display for SpeedMultiplier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A wall-clock snapshot for the panel's date and time command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Time {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// Days since Sunday, matching the panel protocol.
    pub weekday: u8,
}

impl Time {
    /// Snapshot the local clock.
    pub fn now() -> Self {
        let now = Local::now();
        Self {
            year: now.year() as u16,
            month: now.month() as u8,
            day: now.day() as u8,
            hour: now.hour() as u8,
            minute: now.minute() as u8,
            second: now.second() as u8,
            weekday: now.weekday().num_days_from_sunday() as u8,
        }
    }
}

impl Default for Time {
    fn default() -> Self {
        Self {
            year: 2024,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            weekday: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time() -> Time {
        Time {
            year: 2026,
            month: 11,
            day: 18,
            hour: 9,
            minute: 7,
            second: 5,
            weekday: 3,
        }
    }

    #[test]
    fn time_payload_matches_wire_layout() {
        let payloads = PanelCommand::Time {
            format: TimeFormat::H24,
        }
        .payloads(time());

        assert_eq!(
            payloads,
            vec![vec![0x40, 0x09, 0xEA, 0x07, 11, 18, 9, 7, 5, 1, 3]]
        );
    }

    #[test]
    fn power_and_streaming_emit_expected_sequences() {
        assert_eq!(PanelCommand::On.payloads(time()).len(), 2);
        assert_eq!(PanelCommand::Off.payloads(time()).len(), 2);

        assert_eq!(
            PanelCommand::Mode {
                value: ContentMode::Two
            }
            .payloads(time()),
            vec![vec![0x30, 0x06, 0x05, 0x00, 0x00, 0x00, 0x00, 2]]
        );
        assert_eq!(
            PanelCommand::Sweep { value: false }.payloads(time()),
            vec![vec![0x31, 0x02, 0x00, 0x04]]
        );
    }

    #[test]
    fn state_is_a_query_and_has_no_writes() {
        assert!(PanelCommand::State.payloads(time()).is_empty());
        assert_eq!(STATE_QUERY, &[0xf1, 0x03]);
    }

    #[test]
    fn validation_rejects_out_of_range_values() {
        assert!(Fps::new(0).is_err());
        assert!(Fps::new(61).is_err());
        assert_eq!(Fps::new(30).unwrap().get(), 30);

        assert!(SpeedMultiplier::new(-0.1).is_err());
        assert!(SpeedMultiplier::new(f32::NAN).is_err());
        assert!(SpeedMultiplier::new(f32::INFINITY).is_err());
        assert!(SpeedMultiplier::new(1.5).is_ok());

        assert_eq!(Theme::from_value(4), Some(Theme::Four));
        assert_eq!(Theme::from_value(0), None);
        assert_eq!(Speed::from_value(3), Some(Speed::Fast));
        assert_eq!(ContentMode::from_value(4), None);
    }
}
