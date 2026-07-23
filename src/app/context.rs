//! Construction of the bounded context sent to the oracle.

use crate::{memory, oracle, tasks, todos};

const DEFAULT_MEMORY_TURNS: usize = 20;
const MAX_MEMORY_TURNS: usize = 64;

fn memory_turn_limit(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MEMORY_TURNS)
        // ReMagic agent protocol v1 accepts at most 64 explicit history turns.
        .min(MAX_MEMORY_TURNS)
}

/// Build the memory, task, and TODO context sent with a turn.
pub(super) fn build_ctx(
    store: &Option<memory::MemoryStore>,
    task_store: &Option<tasks::TaskStore>,
    todo_store: &Option<todos::TodoStore>,
) -> oracle::TurnContext {
    let configured_turns = std::env::var("MAGICPAPER_MEMORY_TURNS").ok();
    let turns = memory_turn_limit(configured_turns.as_deref());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_turn_setting_is_defaulted_and_bounded_by_the_agent_protocol() {
        assert_eq!(memory_turn_limit(None), DEFAULT_MEMORY_TURNS);
        assert_eq!(
            memory_turn_limit(Some("not-a-number")),
            DEFAULT_MEMORY_TURNS
        );
        assert_eq!(memory_turn_limit(Some("40")), 40);
        assert_eq!(memory_turn_limit(Some("999")), MAX_MEMORY_TURNS);
    }
}
