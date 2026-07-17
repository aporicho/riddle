//! Persistent unscheduled TODO notes, separate from recurring tasks.

use std::io;
use std::path::PathBuf;

const MAX_TODOS: usize = 20;

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
        match std::env::var("RIDDLE_TODOS").as_deref() {
            Ok("off") | Ok("0") | Ok("no") | Ok("false") => return None,
            _ => {}
        }
        let dir = std::env::var("RIDDLE_TODOS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/home/root/riddle-data/todos"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!("magic-paper: TODOs disabled ({}: {e})", dir.display());
            return None;
        }
        let mut store = Self {
            dir,
            entries: Vec::new(),
        };
        store.load();
        Some(store)
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join("index.tsv")
    }

    fn load(&mut self) {
        let Ok(text) = std::fs::read_to_string(self.index_path()) else {
            return;
        };
        for line in text.lines() {
            let Some((id, text)) = line.split_once('\t') else {
                continue;
            };
            let Ok(id) = id.parse() else {
                continue;
            };
            self.entries.push(Todo {
                id,
                text: unescape(text),
            });
            if self.entries.len() == MAX_TODOS {
                break;
            }
        }
    }

    fn persist(&self) -> io::Result<()> {
        let mut out = String::new();
        for todo in &self.entries {
            out.push_str(&format!("{}\t{}\n", todo.id, escape(&todo.text)));
        }
        let tmp = self.dir.join("index.tsv.new");
        std::fs::write(&tmp, out)?;
        std::fs::rename(tmp, self.index_path())
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
        if self.entries.len() >= MAX_TODOS {
            return Err(format!("at most {MAX_TODOS} TODOs are allowed"));
        }
        let id = self
            .entries
            .iter()
            .map(|todo| todo.id)
            .max()
            .map(|id| id.saturating_add(1))
            .unwrap_or(now)
            .max(now);
        let todo = Todo { id, text };
        self.entries.push(todo.clone());
        if let Err(e) = self.persist() {
            self.entries.pop();
            return Err(format!("save TODO: {e}"));
        }
        Ok(Some(todo))
    }

    pub fn delete_number(&mut self, number: usize) -> Result<Todo, String> {
        if number == 0 || number > self.entries.len() {
            return Err(format!(
                "TODO {number} does not exist (there are {})",
                self.entries.len()
            ));
        }
        let index = number - 1;
        let todo = self.entries.remove(index);
        if let Err(e) = self.persist() {
            self.entries.insert(index, todo.clone());
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
    let Some(prefix) = text.get(..4) else {
        return None;
    };
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

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
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
        reopened.load();
        assert_eq!(reopened.entries, store.entries);
        assert_eq!(reopened.panel_lines(), vec!["1  給媽媽打電話"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
