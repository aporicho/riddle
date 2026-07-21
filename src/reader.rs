//! Local book discovery and the managed-runtime handoff to KOReader.
//!
//! reMarkable stores the human title in `<uuid>.metadata` and the book itself
//! under the same UUID.  KOReader, on the other hand, wants a real path.  This
//! module joins the two without asking the oracle or modifying xochitl's data.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const REMARKABLE_LIBRARY: &str = "/home/root/.local/share/remarkable/xochitl";
pub const KOREADER_LIBRARY: &str = "/home/root/koreader";
const MAX_CANDIDATES: usize = 9;
const MAX_WALK_DEPTH: usize = 8;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Book {
    pub title: String,
    pub path: PathBuf,
    pub source: Source,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Remarkable,
    Koreader,
}

impl Book {
    pub fn panel_label(&self) -> String {
        let kind = self
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_uppercase();
        if kind.is_empty() {
            self.title.clone()
        } else {
            format!("{}  ·  {}", self.title, kind)
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    Open(PathBuf),
    Choose(Vec<Book>),
    Missing,
}

#[derive(Debug)]
pub struct Catalog {
    books: Vec<Book>,
    remarkable_root: PathBuf,
    koreader_root: PathBuf,
}

impl Catalog {
    pub fn open() -> io::Result<Self> {
        let (remarkable, koreader) = library_roots();
        Self::scan(remarkable, koreader)
    }

    pub fn scan(
        remarkable_root: impl AsRef<Path>,
        koreader_root: impl AsRef<Path>,
    ) -> io::Result<Self> {
        let remarkable_root = remarkable_root.as_ref().to_path_buf();
        let koreader_root = koreader_root.as_ref().to_path_buf();
        let mut books = Vec::new();
        scan_remarkable(&remarkable_root, &mut books)?;
        scan_koreader(&koreader_root, &mut books)?;

        let mut paths = HashSet::new();
        books.retain(|book| paths.insert(book.path.clone()));
        books.sort_by(|left, right| {
            normalize(&left.title)
                .cmp(&normalize(&right.title))
                .then_with(|| left.title.cmp(&right.title))
                .then_with(|| left.path.cmp(&right.path))
        });

        Ok(Self {
            books,
            remarkable_root,
            koreader_root,
        })
    }

    /// A bare `read` always opens the actual directory, not KOReader's last
    /// book.  Prefer the stock reMarkable library because that is where the
    /// user's existing books live; retain the standalone KOReader directory as
    /// a fallback for installations that keep a separate library.
    pub fn lookup(&self, query: Option<&str>) -> Lookup {
        let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) else {
            let root = if self.remarkable_root.is_dir() {
                &self.remarkable_root
            } else {
                &self.koreader_root
            };
            return if root.is_dir() {
                Lookup::Open(root.clone())
            } else {
                Lookup::Missing
            };
        };

        let needle = normalize(query);
        if needle.is_empty() {
            return Lookup::Missing;
        }

        let exact: Vec<Book> = self
            .books
            .iter()
            .filter(|book| normalize(&book.title) == needle)
            .cloned()
            .collect();
        if !exact.is_empty() {
            return collapse(exact);
        }

        let contained: Vec<Book> = self
            .books
            .iter()
            .filter(|book| {
                let title = normalize(&book.title);
                title.contains(&needle) || needle.contains(&title)
            })
            .cloned()
            .collect();
        if !contained.is_empty() {
            return collapse(contained);
        }

        let needle_chars: Vec<char> = needle.chars().collect();
        let threshold = (needle_chars.len() / 4).clamp(1, 3);
        let mut fuzzy: Vec<(usize, Book)> = self
            .books
            .iter()
            .filter_map(|book| {
                let distance = edit_distance(&needle, &normalize(&book.title));
                (distance <= threshold).then(|| (distance, book.clone()))
            })
            .collect();
        fuzzy.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.title.cmp(&right.1.title))
        });
        collapse(fuzzy.into_iter().map(|(_, book)| book).collect())
    }
}

fn collapse(mut books: Vec<Book>) -> Lookup {
    if books.len() == 1 {
        Lookup::Open(books.remove(0).path)
    } else if books.is_empty() {
        Lookup::Missing
    } else {
        books.truncate(MAX_CANDIDATES);
        Lookup::Choose(books)
    }
}

fn scan_remarkable(root: &Path, books: &mut Vec<Book>) -> io::Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries.flatten() {
        let metadata_path = entry.path();
        if metadata_path.extension().and_then(|value| value.to_str()) != Some("metadata") {
            continue;
        }
        let Some(uuid) = metadata_path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if root.join(format!("{uuid}.tombstone")).exists() {
            continue;
        }
        let Ok(metadata) = fs::read_to_string(&metadata_path) else {
            continue;
        };
        if json_string_field(&metadata, "type").as_deref() != Some("DocumentType") {
            continue;
        }
        let Some(title) = json_string_field(&metadata, "visibleName")
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty())
        else {
            continue;
        };
        // An imported EPUB may also have a generated PDF beside it.  KOReader
        // should receive the reflowable original whenever it exists.
        let path = ["epub", "pdf"]
            .into_iter()
            .map(|extension| root.join(format!("{uuid}.{extension}")))
            .find(|path| path.is_file());
        if let Some(path) = path {
            books.push(Book {
                title,
                path,
                source: Source::Remarkable,
            });
        }
    }
    Ok(())
}

fn scan_koreader(root: &Path, books: &mut Vec<Book>) -> io::Result<()> {
    if !root.exists() {
        return Ok(());
    }
    walk_koreader(root, 0, books)
}

fn walk_koreader(dir: &Path, depth: usize, books: &mut Vec<Book>) -> io::Result<()> {
    if depth > MAX_WALK_DEPTH {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk_koreader(&path, depth + 1, books)?;
            continue;
        }
        if !kind.is_file() || !is_supported(&path) {
            continue;
        }
        let Some(title) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        books.push(Book {
            title: title.to_string(),
            path,
            source: Source::Koreader,
        });
    }
    Ok(())
}

fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| matches!(extension.to_ascii_lowercase().as_str(), "epub" | "pdf"))
}

fn normalize(text: &str) -> String {
    text.chars()
        .filter(|character| character.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, left_character) in left.chars().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(row + 1);
        for (column, right_character) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_character != *right_character);
            current.push(
                (current[column] + 1)
                    .min(previous[column + 1] + 1)
                    .min(substitution),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

/// Parse one JSON string field without bringing a full JSON stack into the
/// takeover binary.  reMarkable metadata is a flat object, but titles may
/// still contain ordinary JSON escapes.
fn json_string_field(document: &str, key: &str) -> Option<String> {
    let marker = format!("\"{key}\"");
    let mut rest = &document[document.find(&marker)? + marker.len()..];
    rest = rest.trim_start();
    rest = rest.strip_prefix(':')?.trim_start();
    rest = rest.strip_prefix('"')?;
    let mut output = String::new();
    let mut chars = rest.chars();
    while let Some(character) = chars.next() {
        match character {
            '"' => return Some(output),
            '\\' => match chars.next()? {
                '"' => output.push('"'),
                '\\' => output.push('\\'),
                '/' => output.push('/'),
                'b' => output.push('\u{0008}'),
                'f' => output.push('\u{000c}'),
                'n' => output.push('\n'),
                'r' => output.push('\r'),
                't' => output.push('\t'),
                'u' => {
                    let hex: String = (0..4).map(|_| chars.next()).collect::<Option<_>>()?;
                    output.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                _ => return None,
            },
            other => output.push(other),
        }
    }
    None
}

/// Canonicalize and constrain a request before it crosses into the application
/// runtime. The manager receives only existing supported books/directories
/// underneath one of MagicPaper's two known libraries.
pub fn validated_target(path: &Path) -> io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if canonical.to_string_lossy().contains(['\n', '\r']) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "KOReader path contains a line break",
        ));
    }
    let (remarkable, koreader) = library_roots();
    let allowed = [remarkable, koreader]
        .into_iter()
        .filter_map(|root| fs::canonicalize(root).ok())
        .any(|root| canonical.starts_with(root));
    if !allowed {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "KOReader path is outside an approved library",
        ));
    }
    if canonical.is_dir() || (canonical.is_file() && is_supported(&canonical)) {
        Ok(canonical)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "KOReader target is not a supported book or directory",
        ))
    }
}

fn library_roots() -> (PathBuf, PathBuf) {
    (
        library_root(
            "RIDDLE_REMARKABLE_LIBRARY",
            "reader/remarkable",
            REMARKABLE_LIBRARY,
        ),
        library_root(
            "RIDDLE_KOREADER_LIBRARY",
            "reader/koreader",
            KOREADER_LIBRARY,
        ),
    )
}

fn library_root(variable: &str, test_child: &str, production: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(variable).filter(|path| !path.is_empty()) {
        return PathBuf::from(path);
    }
    if crate::runtime_env::test_mode() {
        return crate::runtime_env::persistent_path(variable, test_child, production);
    }
    PathBuf::from(production)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("magicpaper-reader-{nonce}-{name}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn add_remarkable(root: &Path, uuid: &str, title: &str, extensions: &[&str]) {
        fs::write(
            root.join(format!("{uuid}.metadata")),
            format!(r#"{{"type":"DocumentType","visibleName":"{title}"}}"#),
        )
        .unwrap();
        for extension in extensions {
            fs::write(root.join(format!("{uuid}.{extension}")), extension).unwrap();
        }
    }

    #[test]
    fn remarkable_titles_map_to_uuid_and_prefer_epub() {
        let rm = temp("rm");
        let ko = temp("ko");
        add_remarkable(&rm, "one", "道德经", &["pdf", "epub"]);
        let catalog = Catalog::scan(&rm, &ko).unwrap();
        assert_eq!(catalog.books.len(), 1);
        assert_eq!(
            catalog.lookup(Some("道德经")),
            Lookup::Open(rm.join("one.epub"))
        );
        fs::remove_dir_all(rm).unwrap();
        fs::remove_dir_all(ko).unwrap();
    }

    #[test]
    fn bare_read_opens_library_and_tombstones_are_hidden() {
        let rm = temp("bare-rm");
        let ko = temp("bare-ko");
        add_remarkable(&rm, "gone", "旧书", &["epub"]);
        fs::write(rm.join("gone.tombstone"), "").unwrap();
        let catalog = Catalog::scan(&rm, &ko).unwrap();
        assert_eq!(catalog.books.len(), 0);
        assert_eq!(catalog.lookup(None), Lookup::Open(rm.clone()));
        fs::remove_dir_all(rm).unwrap();
        fs::remove_dir_all(ko).unwrap();
    }

    #[test]
    fn unique_partial_opens_and_ambiguous_partial_lists_candidates() {
        let rm = temp("match-rm");
        let ko = temp("match-ko");
        add_remarkable(&rm, "up", "诗经 上", &["epub"]);
        add_remarkable(&rm, "down", "诗经 下", &["epub"]);
        add_remarkable(&rm, "dao", "道德经", &["pdf"]);
        let catalog = Catalog::scan(&rm, &ko).unwrap();
        assert_eq!(
            catalog.lookup(Some("道德")),
            Lookup::Open(rm.join("dao.pdf"))
        );
        match catalog.lookup(Some("诗经")) {
            Lookup::Choose(books) => assert_eq!(books.len(), 2),
            other => panic!("expected candidates, got {other:?}"),
        }
        fs::remove_dir_all(rm).unwrap();
        fs::remove_dir_all(ko).unwrap();
    }

    #[test]
    fn fuzzy_matching_tolerates_one_ocr_character_error() {
        let rm = temp("fuzzy-rm");
        let ko = temp("fuzzy-ko");
        add_remarkable(&rm, "book", "置身事内", &["epub"]);
        let catalog = Catalog::scan(&rm, &ko).unwrap();
        assert_eq!(
            catalog.lookup(Some("置身事內")),
            Lookup::Open(rm.join("book.epub"))
        );
        fs::remove_dir_all(rm).unwrap();
        fs::remove_dir_all(ko).unwrap();
    }

    #[test]
    fn standalone_koreader_books_are_recursive() {
        let rm = temp("standalone-rm");
        let ko = temp("standalone-ko");
        fs::create_dir_all(ko.join("小说")).unwrap();
        fs::write(ko.join("小说/蛊真人.epub"), "book").unwrap();
        fs::write(ko.join("小说/ignore.txt"), "text").unwrap();
        let catalog = Catalog::scan(&rm, &ko).unwrap();
        assert_eq!(catalog.books.len(), 1);
        assert_eq!(
            catalog.lookup(Some("蛊真人")),
            Lookup::Open(ko.join("小说/蛊真人.epub"))
        );
        fs::remove_dir_all(rm).unwrap();
        fs::remove_dir_all(ko).unwrap();
    }
}
