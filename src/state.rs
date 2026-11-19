//! Desired panel state, persisted so a reconnect or restart restores it.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::animation::AnimationKind;
use crate::panel::Panel;
use crate::protocol::{ContentMode, Fps, PanelCommand, TimeFormat};
use crate::render::Player;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub animation: AnimationKind,
    pub fps: Fps,
    pub playing: bool,
    /// `None` until the user sets a brightness, so a fresh install leaves the
    /// panel's own default alone.
    pub brightness: Option<u8>,
    pub time_format: TimeFormat,
}

impl Default for State {
    fn default() -> Self {
        Self {
            animation: AnimationKind::default(),
            fps: Fps::default(),
            playing: true,
            brightness: None,
            time_format: TimeFormat::default(),
        }
    }
}

impl State {
    /// Load persisted state, falling back to defaults on a missing or invalid
    /// file. A corrupt file is reported but never stops the daemon.
    pub fn load() -> Self {
        match fs::read(state_path()) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(state) => state,
                Err(error) => {
                    eprintln!("warning: ignoring invalid state file: {error}");
                    Self::default()
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                eprintln!("warning: could not read state file: {error}");
                Self::default()
            }
        }
    }

    /// Persist atomically via a temporary file and rename.
    ///
    /// # Errors
    /// Propagates directory creation, serialization and write failures.
    pub fn save(&self) -> io::Result<()> {
        let path = state_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temporary = path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(self).map_err(io::Error::other)?;
        fs::write(&temporary, bytes)?;
        fs::rename(temporary, path)
    }

    /// Re-apply the desired state to a freshly opened panel.
    ///
    /// # Errors
    /// Propagates panel failures so the caller can drop and retry.
    pub fn apply(&self, panel: &mut dyn Panel, player: &mut Player) -> io::Result<()> {
        panel.apply(&PanelCommand::Time {
            format: self.time_format,
        })?;
        if let Some(brightness) = self.brightness {
            panel.apply(&PanelCommand::Brightness { value: brightness })?;
        }

        player.play(self.animation.clone(), self.fps);
        if self.playing {
            panel.apply(&PanelCommand::Mode {
                value: ContentMode::Two,
            })?;
            panel.apply(&PanelCommand::Sweep { value: false })?;
        } else {
            player.pause();
        }

        Ok(())
    }

    /// A short human-readable summary, used when no panel is connected.
    pub fn summary(&self) -> String {
        format!(
            "{} @ {} fps, {}",
            self.animation.name(),
            self.fps,
            if self.playing { "playing" } else { "paused" }
        )
    }
}

fn state_path() -> PathBuf {
    if let Some(directory) = std::env::var_os("XDG_STATE_HOME") {
        return PathBuf::from(directory).join("zenvision/state.json");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/zenvision/state.json");
    }
    PathBuf::from("zenvision-state.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animation::starfield;
    use crate::protocol::SpeedMultiplier;

    #[test]
    fn state_round_trips_through_json() {
        let state = State {
            animation: AnimationKind::Starfield(starfield::Options {
                stars: 42,
                speed: SpeedMultiplier::new(1.5).unwrap(),
                trail: 50,
            }),
            fps: Fps::new(24).unwrap(),
            playing: false,
            brightness: Some(120),
            time_format: TimeFormat::H12,
        };

        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: State = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, state);
    }

    #[test]
    fn missing_fields_use_defaults() {
        let state: State = serde_json::from_str("{}").unwrap();
        assert_eq!(state, State::default());
        assert!(state.playing);
    }

    #[test]
    fn invalid_fps_is_rejected() {
        let error = serde_json::from_str::<State>(r#"{"fps": 0}"#).unwrap_err();
        assert!(error.to_string().contains("fps"));
    }
}
