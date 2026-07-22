//! Persistent unscheduled TODO notes, separate from recurring tasks.

use crate::storage::persistence::{
    atomic_write, invalid_line, lock_exclusive, read_optional_utf8, unescape_field, StoreLock,
};
use std::collections::HashSet;
use std::io;
use std::path::PathBuf;

const MAX_TODOS: usize = 20;

/// True only for the complete `TODO <text>` add grammar.  In particular,
/// product names such as `todoist` and questions beginning with `todo` are not
/// intercepted by the device-local command router.
pub(crate) fn is_local_add_command(text: &str) -> bool {
    parse_add(text).is_some()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todo {
    pub id: u64,
    pub text: String,
}

pub struct TodoStore {
    dir: PathBuf,
    pub entries: Vec<Todo>,
}

impl TodoStore {
    pub fn open() -> Option<Self> {
        match std::env::var("MAGICPAPER_TODOS").as_deref() {
            Ok("off") | Ok("0") | Ok("no") | Ok("false") => return None,
            _ => {}
        }
        let dir = crate::runtime_env::persistent_path(
            "MAGICPAPER_TODOS_DIR",
            "todos",
            "/home/root/.local/share/magicpaper/todos",
        );
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("magic-paper: TODOs disabled ({}: {e})", dir.display());
            return None;
        }
        let mut store = Self {
            dir,
            entries: Vec::new(),
        };
        match store.load() {
            Ok(()) => Some(store),
            Err(error) => {
                eprintln!(
                    "magic-paper: TODOs disabled because {} could not be loaded: {error}",
                    store.index_path().display()
                );
                None
            }
        }
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.tsv")
    }

    fn lock_exclusive(&self) -> io::Result<StoreLock> {
        lock_exclusive(&self.dir)
    }

    fn load(&mut self) -> io::Result<()> {
        let _lock = self.lock_exclusive()?;
        self.load_unlocked()
    }

    fn load_unlocked(&mut self) -> io::Result<()> {
        let path = self.index_path();
        let Some(contents) = read_optional_utf8(&path)? else {
            self.entries.clear();
            return Ok(());
        };
        let mut entries = Vec::new();
        let mut ids = HashSet::new();
        for (line_index, line) in contents.lines().enumerate() {
            let line_number = line_index + 1;
            if line.is_empty() {
                return Err(invalid_line(&path, line_number, "empty TODO record"));
            }
            let columns = line.split('\t').collect::<Vec<_>>();
            if columns.len() != 2 {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "TODO record must contain exactly two columns",
                ));
            }
            let id = columns[0]
                .parse::<u64>()
                .map_err(|_| invalid_line(&path, line_number, "invalid TODO id"))?;
            if !ids.insert(id) {
                return Err(invalid_line(&path, line_number, "duplicate TODO id"));
            }
            let text = unescape_field(columns[1])
                .map_err(|error| invalid_line(&path, line_number, &error.to_string()))?;
            if text.trim().is_empty() {
                return Err(invalid_line(&path, line_number, "TODO text is empty"));
            }
            entries.push(Todo { id, text });
            if entries.len() > MAX_TODOS {
                return Err(invalid_line(
                    &path,
                    line_number,
                    "TODO store exceeds its maximum size",
                ));
            }
        }
        self.entries = entries;
        Ok(())
    }

    fn persist(&self) -> io::Result<()> {
        let mut out = String::new();
        for todo in &self.entries {
            out.push_str(&format!("{}\t{}\n", todo.id, escape(&todo.text)));
        }
        atomic_write(&self.index_path(), out.as_bytes())
    }

    fn recover_after_failed_persist(&mut self, fallback: Vec<Todo>) {
        if self.load_unlocked().is_err() {
            self.entries = fallback;
        }
    }

    /// `TODO 买牛奶` adds a note. The bare word is reserved for opening the
    /// local list and therefore returns `Ok(None)`.
    pub fn add_from_transcript(
        &mut self,
        transcript: &str,
        now: u64,
    ) -> Result<Option<Todo>, String> {
        let Some(text) = parse_add(transcript) else {
            return Ok(None);
        };
        let _lock = self
            .lock_exclusive()
            .map_err(|error| format!("lock TODO store: {error}"))?;
        self.load_unlocked()
            .map_err(|error| format!("load TODO store: {error}"))?;
        if self.entries.len() >= MAX_TODOS {
            return Err(format!("at most {MAX_TODOS} TODOs are allowed"));
        }
        let id = match self.entries.iter().map(|todo| todo.id).max() {
            Some(previous) => previous
                .checked_add(1)
                .ok_or_else(|| "TODO id space is exhausted".to_string())?
                .max(now),
            None => now,
        };
        let old_entries = self.entries.clone();
        let todo = Todo { id, text };
        self.entries.push(todo.clone());
        if let Err(e) = self.persist() {
            self.recover_after_failed_persist(old_entries);
            return Err(format!("save TODO: {e}"));
        }
        Ok(Some(todo))
    }

    pub fn delete_number(&mut self, number: usize) -> Result<Todo, String> {
        let _lock = self
            .lock_exclusive()
            .map_err(|error| format!("lock TODO store: {error}"))?;
        self.load_unlocked()
            .map_err(|error| format!("load TODO store: {error}"))?;
        if number == 0 || number > self.entries.len() {
            return Err(format!(
                "TODO {number} does not exist (there are {})",
                self.entries.len()
            ));
        }
        let old_entries = self.entries.clone();
        let index = number - 1;
        let todo = self.entries.remove(index);
        if let Err(e) = self.persist() {
            self.recover_after_failed_persist(old_entries);
            return Err(format!("save TODO deletion: {e}"));
        }
        Ok(todo)
    }

    pub fn catalog_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, todo)| format!("{}. {}", index + 1, todo.text))
            .collect()
    }

    pub fn panel_lines(&self) -> Vec<String> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, todo)| format!("{}  {}", index + 1, todo.text))
            .collect()
    }
}

fn parse_add(transcript: &str) -> Option<String> {
    let text = transcript.trim();
    let prefix = text.get(..4)?;
    if !prefix.eq_ignore_ascii_case("todo") {
        return None;
    }
    let rest = &text[4..];
    if rest
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let rest = rest
        .trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ':' | '：' | ',' | '，' | '-' | '—')
        })
        .trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(name: &str) -> TodoStore {
        let dir = std::env::temp_dir().join(format!(
            "magic-paper-todo-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TodoStore {
            dir,
            entries: Vec::new(),
        }
    }

    #[test]
    fn parses_case_variants_and_reserves_bare_todo() {
        assert_eq!(parse_add("TODO 买牛奶"), Some("买牛奶".into()));
        assert_eq!(parse_add("Todo：給媽媽打電話"), Some("給媽媽打電話".into()));
        assert_eq!(parse_add("todo"), None);
        assert_eq!(parse_add("todoist"), None);
    }

    #[test]
    fn persists_and_deletes_by_visible_number() {
        let mut store = tmp_store("round-trip");
        store.add_from_transcript("TODO 买牛奶", 100).unwrap();
        store.add_from_transcript("todo 給媽媽打電話", 101).unwrap();
        assert_eq!(store.panel_lines(), vec!["1  买牛奶", "2  給媽媽打電話"]);
        let deleted = store.delete_number(1).unwrap();
        assert_eq!(deleted.text, "买牛奶");

        let dir = store.dir.clone();
        let mut reopened = TodoStore {
            dir: dir.clone(),
            entries: Vec::new(),
        };
        reopened.load().unwrap();
        assert_eq!(reopened.entries, store.entries);
        assert_eq!(reopened.panel_lines(), vec!["1  給媽媽打電話"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn corrupt_todo_index_is_reported_and_never_overwritten() {
        let mut store = tmp_store("corrupt-index");
        store.add_from_transcript("TODO 买牛奶", 100).unwrap();
        let cached = store.entries.clone();
        let corrupt = b"broken TODO record\n";
        std::fs::write(store.index_path(), corrupt).unwrap();

        let error = store.load().unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(store.entries, cached);
        assert!(store
            .add_from_transcript("TODO 打电话", 101)
            .unwrap_err()
            .contains("load TODO store"));
        assert_eq!(std::fs::read(store.index_path()).unwrap(), corrupt);
        let _ = std::fs::remove_dir_all(store.dir);
    }

    #[test]
    fn non_not_found_todo_read_error_is_not_an_empty_store() {
        let mut store = tmp_store("read-error");
        std::fs::create_dir(store.index_path()).unwrap();
        let error = store.load().unwrap_err();
        assert_ne!(error.kind(), io::ErrorKind::NotFound);
        assert!(store.add_from_transcript("TODO 买牛奶", 100).is_err());
        assert!(store.index_path().is_dir());
        let _ = std::fs::remove_dir_all(store.dir);
    }

    #[test]
    fn failed_atomic_todo_replacement_keeps_original_bytes_and_cache() {
        let mut store = tmp_store("atomic-failure");
        store.add_from_transcript("TODO 买牛奶", 100).unwrap();
        let before = std::fs::read(store.index_path()).unwrap();
        std::fs::create_dir(store.dir.join("index.tsv.new")).unwrap();

        assert!(store
            .add_from_transcript("TODO 打电话", 101)
            .unwrap_err()
            .contains("save TODO"));
        assert_eq!(std::fs::read(store.index_path()).unwrap(), before);
        assert_eq!(store.panel_lines(), vec!["1  买牛奶"]);
        let _ = std::fs::remove_dir_all(store.dir);
    }

    #[test]
    fn concurrent_todo_additions_reload_under_one_lock() {
        let original = tmp_store("concurrent-add");
        let dir = original.dir.clone();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut handles = Vec::new();
        for (command, now) in [("TODO 买牛奶", 100), ("TODO 打电话", 100)] {
            let dir = dir.clone();
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let mut store = TodoStore {
                    dir,
                    entries: Vec::new(),
                };
                barrier.wait();
                store.add_from_transcript(command, now).unwrap().unwrap().id
            }));
        }
        barrier.wait();
        let ids = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();
        assert_ne!(ids[0], ids[1]);
        let mut reopened = TodoStore {
            dir: dir.clone(),
            entries: Vec::new(),
        };
        reopened.load().unwrap();
        assert_eq!(reopened.entries.len(), 2);
        let _ = std::fs::remove_dir_all(dir);
    }
}
