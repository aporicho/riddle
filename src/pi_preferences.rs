//! Durable, non-secret preferences for MagicPaper's Pi agent profile.

use std::fs::OpenOptions;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const DEFAULT_PREF_DIR: &str = "/home/root/.local/share/magicpaper/preferences";
const PI_SETTINGS_FILE: &str = "pi-settings.json";
const PI_SETTINGS_SCHEMA: u32 = 1;
const MAX_PI_SETTINGS_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PiProvider {
    #[default]
    DeepSeek,
    #[serde(rename = "openai", alias = "open_ai_codex")]
    OpenAi,
}

impl PiProvider {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::DeepSeek => "DeepSeek",
            Self::OpenAi => "OpenAI",
        }
    }

    pub(crate) const fn next(self) -> Self {
        match self {
            Self::DeepSeek => Self::OpenAi,
            Self::OpenAi => Self::DeepSeek,
        }
    }

    pub(crate) const fn provider_id(self) -> &'static str {
        match self {
            Self::DeepSeek => "deepseek",
            Self::OpenAi => "openai",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum PiModel {
    #[default]
    #[serde(rename = "deepseek-v4-flash")]
    DeepSeekV4Flash,
    #[serde(rename = "deepseek-v4-pro")]
    DeepSeekV4Pro,
}

impl PiModel {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::DeepSeekV4Flash => "Flash",
            Self::DeepSeekV4Pro => "Pro",
        }
    }

    /// Map the two paper-facing speed/quality choices to a model supported by
    /// the selected provider, so an invalid cross-provider pair is impossible.
    pub(crate) const fn model_id(self, provider: PiProvider) -> &'static str {
        match (provider, self) {
            (PiProvider::DeepSeek, Self::DeepSeekV4Flash) => "deepseek-v4-flash",
            (PiProvider::DeepSeek, Self::DeepSeekV4Pro) => "deepseek-v4-pro",
            (PiProvider::OpenAi, Self::DeepSeekV4Flash) => "gpt-5.6-terra",
            (PiProvider::OpenAi, Self::DeepSeekV4Pro) => "gpt-5.6-sol",
        }
    }

    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::DeepSeekV4Flash => Self::DeepSeekV4Pro,
            Self::DeepSeekV4Pro => Self::DeepSeekV4Flash,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PiThinking {
    #[default]
    Off,
    Low,
    High,
    ExtraHigh,
    Maximum,
}

impl PiThinking {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Off => "关闭",
            Self::Low => "低",
            Self::High => "高",
            Self::ExtraHigh => "极高",
            Self::Maximum => "最大",
        }
    }

    pub(crate) const fn next(self, provider: PiProvider) -> Self {
        match (provider, self) {
            // Pi 0.81 marks DeepSeek V4 minimal/low/medium as unsupported.
            // Keep the paper selector on values the packaged catalog accepts.
            (PiProvider::DeepSeek, Self::Off) => Self::High,
            (PiProvider::DeepSeek, Self::Low | Self::High) => Self::Maximum,
            (PiProvider::DeepSeek, Self::ExtraHigh) => Self::Off,
            (PiProvider::DeepSeek, Self::Maximum) => Self::Off,
            // GPT-5.6 Terra/Sol expose only off, xhigh and max in Pi 0.81.
            (PiProvider::OpenAi, Self::Off) => Self::ExtraHigh,
            (PiProvider::OpenAi, Self::Low | Self::High) => Self::Off,
            (PiProvider::OpenAi, Self::ExtraHigh) => Self::Maximum,
            (PiProvider::OpenAi, Self::Maximum) => Self::Off,
        }
    }

    pub(crate) const fn normalized(self, provider: PiProvider) -> Self {
        match (provider, self) {
            (PiProvider::DeepSeek, Self::Low | Self::ExtraHigh) => Self::Off,
            (PiProvider::OpenAi, Self::Low | Self::High) => Self::Off,
            _ => self,
        }
    }

    pub(crate) const fn thinking_id(self, provider: PiProvider) -> &'static str {
        match self.normalized(provider) {
            Self::Off => "off",
            Self::Low => "low",
            Self::High => "high",
            Self::ExtraHigh => "xhigh",
            Self::Maximum => "max",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct PiPreferenceValues {
    pub(crate) provider: PiProvider,
    pub(crate) model: PiModel,
    pub(crate) thinking: PiThinking,
    pub(crate) tools_enabled: bool,
}

impl Default for PiPreferenceValues {
    fn default() -> Self {
        Self {
            provider: PiProvider::DeepSeek,
            model: PiModel::DeepSeekV4Flash,
            thinking: PiThinking::Off,
            tools_enabled: true,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(default)]
struct StoredPiPreferences {
    schema: u32,
    values: PiPreferenceValues,
}

impl Default for StoredPiPreferences {
    fn default() -> Self {
        Self {
            schema: PI_SETTINGS_SCHEMA,
            values: PiPreferenceValues::default(),
        }
    }
}

pub(crate) struct PiPreferences {
    path: PathBuf,
    values: PiPreferenceValues,
}

impl PiPreferences {
    pub(crate) fn open() -> Self {
        let dir = crate::runtime_env::persistent_path(
            "MAGICPAPER_PREFERENCES_DIR",
            "preferences",
            DEFAULT_PREF_DIR,
        );
        Self::open_in(&dir)
    }

    fn open_in(dir: &Path) -> Self {
        let path = dir.join(PI_SETTINGS_FILE);
        let mut values = match read_bounded(&path) {
            Ok(bytes) => match serde_json::from_slice::<StoredPiPreferences>(&bytes) {
                Ok(stored) if stored.schema == PI_SETTINGS_SCHEMA => stored.values,
                Ok(stored) => {
                    eprintln!(
                        "magic-paper: unsupported Pi settings schema {}; using defaults",
                        stored.schema
                    );
                    PiPreferenceValues::default()
                }
                Err(error) => {
                    eprintln!(
                        "magic-paper: could not parse Pi settings at {}: {error}; using defaults",
                        path.display()
                    );
                    PiPreferenceValues::default()
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => PiPreferenceValues::default(),
            Err(error) => {
                eprintln!(
                    "magic-paper: could not read Pi settings at {}: {error}; using defaults",
                    path.display()
                );
                PiPreferenceValues::default()
            }
        };
        values.thinking = values.thinking.normalized(values.provider);
        Self { path, values }
    }

    pub(crate) const fn values(&self) -> PiPreferenceValues {
        self.values
    }

    pub(crate) fn replace(&mut self, values: PiPreferenceValues) -> io::Result<()> {
        let previous = self.values;
        self.values = values;
        if let Err(error) = self.persist() {
            self.values = previous;
            return Err(error);
        }
        Ok(())
    }

    fn persist(&self) -> io::Result<()> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("Pi settings path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let temporary = parent.join("pi-settings.json.new");
        match std::fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let body = serde_json::to_vec_pretty(&StoredPiPreferences {
            schema: PI_SETTINGS_SCHEMA,
            values: self.values,
        })
        .map_err(io::Error::other)?;
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::rename(&temporary, &self.path)?;
        sync_directory(parent)
    }
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let mut bytes = Vec::with_capacity(1024);
    file.take((MAX_PI_SETTINGS_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_PI_SETTINGS_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Pi settings exceed 64 KiB",
        ));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "pi_preferences/tests.rs"]
mod tests;
