//! Durable paper-facing experience preferences.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DEFAULT_PREF_DIR: &str = "/home/root/riddle-data/preferences";
const SETTINGS_FILE: &str = "settings.json";
const SETTINGS_SCHEMA: u32 = 1;

pub(crate) const MIN_CLEANUP_PADDING_PX: u8 = 0;
pub(crate) const MAX_CLEANUP_PADDING_PX: u8 = 32;
pub(crate) const CLEANUP_PADDING_STEP_PX: u8 = 4;
pub(crate) const MIN_FULL_REFRESH_INTERVAL: u8 = 0;
pub(crate) const MAX_FULL_REFRESH_INTERVAL: u8 = 10;
pub(crate) const MIN_ANSWER_DWELL_PERCENT: u16 = 50;
pub(crate) const MAX_ANSWER_DWELL_PERCENT: u16 = 200;
pub(crate) const ANSWER_DWELL_STEP_PERCENT: u16 = 10;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CleanupStrength {
    Standard,
    #[default]
    Enhanced,
}

impl CleanupStrength {
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Standard => Self::Enhanced,
            Self::Enhanced => Self::Standard,
        }
    }

    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Standard => "标准",
            Self::Enhanced => "增强",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PreferenceValues {
    pub(crate) cleanup_strength: CleanupStrength,
    pub(crate) cleanup_padding_px: u8,
    pub(crate) full_refresh_every_replies: u8,
    pub(crate) answer_dwell_percent: u16,
}

impl Default for PreferenceValues {
    fn default() -> Self {
        Self {
            cleanup_strength: CleanupStrength::Enhanced,
            cleanup_padding_px: 16,
            full_refresh_every_replies: 3,
            answer_dwell_percent: 100,
        }
    }
}

impl PreferenceValues {
    fn normalized(mut self) -> Self {
        self.cleanup_padding_px = quantize_u8(
            self.cleanup_padding_px
                .clamp(MIN_CLEANUP_PADDING_PX, MAX_CLEANUP_PADDING_PX),
            CLEANUP_PADDING_STEP_PX,
        );
        self.full_refresh_every_replies = self
            .full_refresh_every_replies
            .clamp(MIN_FULL_REFRESH_INTERVAL, MAX_FULL_REFRESH_INTERVAL);
        self.answer_dwell_percent = quantize_u16(
            self.answer_dwell_percent
                .clamp(MIN_ANSWER_DWELL_PERCENT, MAX_ANSWER_DWELL_PERCENT),
            ANSWER_DWELL_STEP_PERCENT,
        );
        self
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct StoredPreferences {
    schema: u32,
    values: PreferenceValues,
}

impl Default for StoredPreferences {
    fn default() -> Self {
        Self {
            schema: SETTINGS_SCHEMA,
            values: PreferenceValues::default(),
        }
    }
}

pub(crate) struct UserPreferences {
    path: PathBuf,
    values: PreferenceValues,
}

impl UserPreferences {
    pub(crate) fn open() -> Self {
        let dir = crate::runtime_env::persistent_path(
            "RIDDLE_PREFERENCES_DIR",
            "preferences",
            DEFAULT_PREF_DIR,
        );
        Self::open_in(&dir)
    }

    fn open_in(dir: &Path) -> Self {
        let path = dir.join(SETTINGS_FILE);
        let values = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoredPreferences>(&bytes) {
                Ok(stored) if stored.schema == SETTINGS_SCHEMA => stored.values.normalized(),
                Ok(stored) => {
                    eprintln!(
                        "magic-paper: unsupported settings schema {}; using defaults",
                        stored.schema
                    );
                    PreferenceValues::default()
                }
                Err(error) => {
                    eprintln!(
                        "magic-paper: could not parse settings at {}: {error}; using defaults",
                        path.display()
                    );
                    PreferenceValues::default()
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => PreferenceValues::default(),
            Err(error) => {
                eprintln!(
                    "magic-paper: could not read settings at {}: {error}; using defaults",
                    path.display()
                );
                PreferenceValues::default()
            }
        };
        Self { path, values }
    }

    pub(crate) const fn values(&self) -> PreferenceValues {
        self.values
    }

    pub(crate) fn replace(&mut self, values: PreferenceValues) -> io::Result<()> {
        self.values = values.normalized();
        self.persist()
    }

    fn persist(&self) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("settings path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join("settings.json.new");
        let body = serde_json::to_vec_pretty(&StoredPreferences {
            schema: SETTINGS_SCHEMA,
            values: self.values,
        })
        .map_err(io::Error::other)?;
        std::fs::write(&temporary, body)?;
        std::fs::rename(temporary, &self.path)
    }

    #[cfg(test)]
    pub(crate) fn for_test(values: PreferenceValues) -> Self {
        Self {
            path: PathBuf::new(),
            values: values.normalized(),
        }
    }
}

fn quantize_u8(value: u8, step: u8) -> u8 {
    ((value as u16 + step as u16 / 2) / step as u16 * step as u16) as u8
}

fn quantize_u16(value: u16, step: u16) -> u16 {
    (value + step / 2) / step * step
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "magicpaper-preferences-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn missing_and_corrupt_settings_fall_back_to_balanced_defaults() {
        let dir = temp_dir("defaults");
        let preferences = UserPreferences::open_in(&dir);
        assert_eq!(preferences.values(), PreferenceValues::default());

        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(SETTINGS_FILE), b"not-json").unwrap();
        let preferences = UserPreferences::open_in(&dir);
        assert_eq!(preferences.values(), PreferenceValues::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_are_clamped_quantized_and_round_trip_atomically() {
        let dir = temp_dir("round-trip");
        let mut preferences = UserPreferences::open_in(&dir);
        preferences
            .replace(PreferenceValues {
                cleanup_strength: CleanupStrength::Standard,
                cleanup_padding_px: 31,
                full_refresh_every_replies: 99,
                answer_dwell_percent: 146,
            })
            .unwrap();
        assert_eq!(preferences.values().cleanup_padding_px, 32);
        assert_eq!(preferences.values().full_refresh_every_replies, 10);
        assert_eq!(preferences.values().answer_dwell_percent, 150);

        let reopened = UserPreferences::open_in(&dir);
        assert_eq!(reopened.values(), preferences.values());
        assert!(!dir.join("settings.json.new").exists());
        let _ = std::fs::remove_dir_all(dir);
    }
}
