//! Runtime-selectable handwriting fonts with per-glyph fallback.
//!
//! ChenYuluoyan is embedded because it is redistributable. The two locally
//! supplied fonts live beside the application under `fonts/`; keeping them out
//! of the executable and repository preserves their local-only status.

use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontRef};

const CHEN_BYTES: &[u8] = include_bytes!("../../fonts/ChenYuluoyan-2.0-Thin.ttf");
const DEFAULT_PREF_DIR: &str = "/home/root/riddle-data/preferences";
const FONT_PREF_FILE: &str = "font";
const FONT_SCALE_PREF_FILE: &str = "font_scales";
pub const MIN_SCALE_PERCENT: u16 = 50;
pub const MAX_SCALE_PERCENT: u16 = 180;
const DEFAULT_SCALE_PERCENT: u16 = 100;

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

    const fn index(self) -> usize {
        match self {
            Self::ChenYuluoyan => 0,
            Self::ButterShisan => 1,
            Self::Farstar851 => 2,
        }
    }
}

struct FontEntry {
    id: FontId,
    font: FontRef<'static>,
}

pub struct FontBook {
    entries: Vec<FontEntry>,
    selected: FontId,
    scales: [u16; 3],
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
        let scales = load_scales(&preference_dir);

        eprintln!(
            "magic-paper: fonts={} selected={} scales={}/{}/{}",
            entries
                .iter()
                .map(|entry| entry.id.stable_id())
                .collect::<Vec<_>>()
                .join(","),
            selected.stable_id(),
            scales[0],
            scales[1],
            scales[2]
        );
        Ok(Self {
            entries,
            selected,
            scales,
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

    pub fn scale_percent(&self, id: FontId) -> u16 {
        self.scales[id.index()]
    }

    /// Convert a semantic text size into the calibrated pixel size for the
    /// font that will actually draw the glyph.
    pub fn calibrated_px(&self, id: FontId, base_px: f32) -> f32 {
        base_px * self.scale_percent(id) as f32 / 100.0
    }

    /// Apply one font's visual-size calibration immediately and persist all
    /// three stable IDs atomically. Values outside the paper UI's supported
    /// range are clamped so hand-edited preference files remain safe.
    pub fn set_scale_percent(&mut self, id: FontId, percent: u16) -> std::io::Result<()> {
        self.scales[id.index()] = percent.clamp(MIN_SCALE_PERCENT, MAX_SCALE_PERCENT);
        std::fs::create_dir_all(&self.preference_dir)?;
        let target = self.preference_dir.join(FONT_SCALE_PREF_FILE);
        let temporary = self.preference_dir.join("font_scales.new");
        let mut body = String::new();
        for font_id in FontId::ALL {
            body.push_str(font_id.stable_id());
            body.push('=');
            body.push_str(&self.scale_percent(font_id).to_string());
            body.push('\n');
        }
        std::fs::write(&temporary, body)?;
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
            scales: [DEFAULT_SCALE_PERCENT; 3],
            preference_dir: std::env::temp_dir(),
        }
    }

    #[cfg(test)]
    pub fn set_scale_for_test(&mut self, id: FontId, percent: u16) {
        self.scales[id.index()] = percent.clamp(MIN_SCALE_PERCENT, MAX_SCALE_PERCENT);
    }
}

fn load_scales(preference_dir: &Path) -> [u16; 3] {
    let mut scales = [DEFAULT_SCALE_PERCENT; 3];
    let Ok(saved) = std::fs::read_to_string(preference_dir.join(FONT_SCALE_PREF_FILE)) else {
        return scales;
    };
    for line in saved.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let Some(id) = FontId::parse(key) else {
            continue;
        };
        let Ok(percent) = value.trim().parse::<u16>() else {
            continue;
        };
        scales[id.index()] = percent.clamp(MIN_SCALE_PERCENT, MAX_SCALE_PERCENT);
    }
    scales
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
        let latin =
            FontRef::try_from_slice(include_bytes!("../../fonts/DancingScript.ttf")).unwrap();
        let chinese =
            FontRef::try_from_slice(include_bytes!("../../fonts/ChenYuluoyan-2.0-Thin.ttf"))
                .unwrap();
        let book = FontBook::for_test(latin, Some(chinese));
        assert_eq!(
            book.resolve(FontId::ChenYuluoyan, '務').0,
            FontId::Farstar851
        );
    }

    #[test]
    fn selection_is_written_with_a_stable_id() {
        let font = FontRef::try_from_slice(include_bytes!("../../fonts/ChenYuluoyan-2.0-Thin.ttf"))
            .unwrap();
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

    #[test]
    fn scale_calibration_is_clamped_and_persisted_by_font() {
        let font = FontRef::try_from_slice(include_bytes!("../../fonts/ChenYuluoyan-2.0-Thin.ttf"))
            .unwrap();
        let mut book = FontBook::for_test(font.clone(), Some(font));
        let dir = std::env::temp_dir().join(format!(
            "magic-paper-font-scale-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        book.preference_dir = dir.clone();
        book.set_scale_percent(FontId::ChenYuluoyan, 25).unwrap();
        book.set_scale_percent(FontId::Farstar851, 147).unwrap();
        assert_eq!(book.scale_percent(FontId::ChenYuluoyan), MIN_SCALE_PERCENT);
        assert_eq!(book.scale_percent(FontId::Farstar851), 147);
        assert_eq!(book.calibrated_px(FontId::Farstar851, 100.0), 147.0);
        let saved = std::fs::read_to_string(dir.join(FONT_SCALE_PREF_FILE)).unwrap();
        assert!(saved.contains("chenyuluoyan=50\n"));
        assert!(saved.contains("851_farstar=147\n"));
        let reloaded = load_scales(&dir);
        assert_eq!(reloaded[FontId::ChenYuluoyan.index()], 50);
        assert_eq!(reloaded[FontId::Farstar851.index()], 147);
        let _ = std::fs::remove_dir_all(dir);
    }
}
