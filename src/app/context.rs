//! Construction of the bounded context sent to the oracle.

use crate::{memory, oracle, tasks, todos};

/// Build the memory, task, and TODO context sent with a turn.
pub(super) fn build_ctx(
    store: &Option<memory::MemoryStore>,
    task_store: &Option<tasks::TaskStore>,
    todo_store: &Option<todos::TodoStore>,
) -> oracle::TurnContext {
    let turns: usize = std::env::var("RIDDLE_MEMORY_TURNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let (history, catalog_lines, catalog_ids) = match store {
        Some(s) => {
            let (lines, ids) = s.catalog(40);
            (s.recent_dialogue(turns), lines, ids)
        }
        None => (Vec::new(), Vec::new(), Vec::new()),
    };
    let task_lines = task_store
        .as_ref()
        .map(|s| s.catalog_lines())
        .unwrap_or_default();
    let todo_lines = todo_store
        .as_ref()
        .map(|s| s.catalog_lines())
        .unwrap_or_default();
    oracle::TurnContext {
        history,
        catalog_lines,
        catalog_ids,
        task_lines,
        todo_lines,
    }
}
