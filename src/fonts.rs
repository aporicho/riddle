//! Runtime-selectable handwriting fonts with per-glyph fallback.
//!
//! ChenYuluoyan is embedded because it is redistributable. The two locally
//! supplied fonts live beside the application under `fonts/`; keeping them out
//! of the executable and repository preserves their local-only status.

use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontRef};

const CHEN_BYTES: &[u8] = include_bytes!("../fonts/ChenYuluoyan-2.0-Thin.ttf");
const DEFAULT_PREF_DIR: &str = "/home/root/riddle-data/preferences";
const FONT_PREF_FILE: &str = "font";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FontId {
    ChenYuluoyan,
    ButterShisan,
    Farstar851,
}

impl FontId {
    pub const ALL: [Self; 3] = [Self::ChenYuluoyan, Self::ButterShisan, Self::Farstar851];

    pub const fn stable_id(self) -> &'static str {
        match self {
            Self::ChenYuluoyan => "chenyuluoyan",
            Self::ButterShisan => "butter_shisan",
            Self::Farstar851 => "851_farstar",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ChenYuluoyan => "辰宇落雁体",
            Self::ButterShisan => "黄油拾叁体",
            Self::Farstar851 => "851 远星夜行手写体",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|id| id.stable_id() == value.trim())
    }
}

struct FontEntry {
    id: FontId,
    font: FontRef<'static>,
}

pub struct FontBook {
    entries: Vec<FontEntry>,
    selected: FontId,
    preference_dir: PathBuf,
}

impl FontBook {
    pub fn open() -> std::io::Result<Self> {
        let chen = FontRef::try_from_slice(CHEN_BYTES).map_err(std::io::Error::other)?;
        let mut entries = vec![FontEntry {
            id: FontId::ChenYuluoyan,
            font: chen,
        }];

        let font_dir = runtime_font_dir();
        load_optional(
            &mut entries,
            FontId::ButterShisan,
            &font_dir.join("ButterShiSan.ttf"),
        );
        load_optional(
            &mut entries,
            FontId::Farstar851,
            &font_dir.join("851LakeusNightWriting.ttf"),
        );

        let preference_dir = std::env::var_os("RIDDLE_PREFERENCES_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PREF_DIR));
        let default = if entries.iter().any(|entry| entry.id == FontId::Farstar851) {
            FontId::Farstar851
        } else {
            FontId::ChenYuluoyan
        };
        let selected = std::fs::read_to_string(preference_dir.join(FONT_PREF_FILE))
            .ok()
            .and_then(|saved| FontId::parse(&saved))
            .filter(|id| entries.iter().any(|entry| entry.id == *id))
            .unwrap_or(default);

        eprintln!(
            "magic-paper: fonts={} selected={}",
            entries
                .iter()
                .map(|entry| entry.id.stable_id())
                .collect::<Vec<_>>()
                .join(","),
            selected.stable_id()
        );
        Ok(Self {
            entries,
            selected,
            preference_dir,
        })
    }

    pub fn selected(&self) -> FontId {
        self.selected
    }

    pub fn available(&self) -> impl Iterator<Item = FontId> + '_ {
        FontId::ALL.into_iter().filter(|id| self.has(*id))
    }

    pub fn has(&self, id: FontId) -> bool {
        self.entries.iter().any(|entry| entry.id == id)
    }

    /// Apply immediately, then persist atomically. A storage error does not
    /// undo the visible selection for the current session.
    pub fn select(&mut self, id: FontId) -> std::io::Result<()> {
        if !self.has(id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("font {} is not installed", id.stable_id()),
            ));
        }
        self.selected = id;
        std::fs::create_dir_all(&self.preference_dir)?;
        let target = self.preference_dir.join(FONT_PREF_FILE);
        let temporary = self.preference_dir.join("font.new");
        std::fs::write(&temporary, format!("{}\n", id.stable_id()))?;
        std::fs::rename(temporary, target)
    }

    pub fn font(&self, id: FontId) -> &FontRef<'static> {
        self.entries
            .iter()
            .find(|entry| entry.id == id)
            .or_else(|| self.entries.first())
            .map(|entry| &entry.font)
            .expect("FontBook always contains ChenYuluoyan")
    }

    /// Resolve a character using the requested style first, then 851 as the
    /// broad Simplified/Traditional fallback, then the remaining local fonts.
    pub fn resolve(&self, primary: FontId, c: char) -> (FontId, &FontRef<'static>) {
        let order = [
            primary,
            FontId::Farstar851,
            FontId::ButterShisan,
            FontId::ChenYuluoyan,
        ];
        for id in order {
            if let Some(entry) = self.entries.iter().find(|entry| entry.id == id) {
                if entry.font.glyph_id(c).0 != 0 {
                    return (id, &entry.font);
                }
            }
        }
        (primary, self.font(primary))
    }

    #[cfg(test)]
    pub fn for_test(primary: FontRef<'static>, fallback: Option<FontRef<'static>>) -> Self {
        let mut entries = vec![FontEntry {
            id: FontId::ChenYuluoyan,
            font: primary,
        }];
        if let Some(font) = fallback {
            entries.push(FontEntry {
                id: FontId::Farstar851,
                font,
            });
        }
        Self {
            entries,
            selected: FontId::ChenYuluoyan,
            preference_dir: std::env::temp_dir(),
        }
    }
}

fn runtime_font_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("RIDDLE_FONT_DIR") {
        return PathBuf::from(dir);
    }
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
        .join("fonts")
}

fn load_optional(entries: &mut Vec<FontEntry>, id: FontId, path: &Path) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "magic-paper: optional font {} unavailable at {} ({error})",
                id.stable_id(),
                path.display()
            );
            return;
        }
    };
    // Font data is needed for the process lifetime. Leaking these two buffers
    // avoids a self-referential owner/parser structure and costs no recurring
    // memory; the OS reclaims it when MagicPaper exits.
    let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    match FontRef::try_from_slice(bytes) {
        Ok(font) => entries.push(FontEntry { id, font }),
        Err(error) => eprintln!(
            "magic-paper: optional font {} is invalid at {} ({error})",
            id.stable_id(),
            path.display()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_glyph_uses_the_broad_fallback() {
        let latin = FontRef::try_from_slice(include_bytes!("../fonts/DancingScript.ttf")).unwrap();
        let chinese =
            FontRef::try_from_slice(include_bytes!("../fonts/ChenYuluoyan-2.0-Thin.ttf")).unwrap();
        let book = FontBook::for_test(latin, Some(chinese));
        assert_eq!(
            book.resolve(FontId::ChenYuluoyan, '務').0,
            FontId::Farstar851
        );
    }

    #[test]
    fn selection_is_written_with_a_stable_id() {
        let font =
            FontRef::try_from_slice(include_bytes!("../fonts/ChenYuluoyan-2.0-Thin.ttf")).unwrap();
        let mut book = FontBook::for_test(font.clone(), Some(font));
        let dir =
            std::env::temp_dir().join(format!("magic-paper-font-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        book.preference_dir = dir.clone();
        book.select(FontId::Farstar851).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join(FONT_PREF_FILE)).unwrap(),
            "851_farstar\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
