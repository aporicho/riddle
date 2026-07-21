use super::*;

fn tmp_store(name: &str) -> MemoryStore {
    let dir = std::env::temp_dir().join(format!("riddle-mem-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    MemoryStore {
        dir,
        entries: Vec::new(),
    }
}

#[test]
fn round_trip_and_reload() {
    let mut store = tmp_store("rt");
    let strokes: Strokes = vec![vec![(10, 20, 3), (14, 24, 3), (100, 120, 2)]];
    store.append(
        1751856000,
        "hello\ttom\nnewline",
        "Hello. Who writes?",
        &strokes,
    );
    let mut reopened = MemoryStore {
        dir: store.dir.clone(),
        entries: Vec::new(),
    };
    reopened.load();
    assert_eq!(reopened.entries.len(), 1);
    assert_eq!(reopened.entries[0].transcript, "hello\ttom\nnewline");
    assert_eq!(reopened.entries[0].reply, "Hello. Who writes?");
    let back = reopened.strokes(1751856000).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].first(), Some(&(10, 20, 3)));
    assert_eq!(back[0].last(), Some(&(100, 120, 2)));
    let _ = std::fs::remove_dir_all(&reopened.dir);
}

#[test]
fn decimation_keeps_endpoints_drops_dense() {
    let dense: Strokes = vec![(0..100).map(|i| (i, 0, 3)).collect()];
    let thin = decimate(&dense);
    assert!(thin[0].len() < 40, "kept too many: {}", thin[0].len());
    assert_eq!(thin[0].first(), Some(&(0, 0, 3)));
    assert_eq!(thin[0].last(), Some(&(99, 0, 3)));
}

#[test]
fn prune_forgets_oldest() {
    let mut store = tmp_store("prune");
    for id in 1..=(MAX_MEMORIES + 5) as u64 {
        store.append(id, "t", "r", &vec![vec![(1, 1, 1)]]);
    }
    assert_eq!(store.entries.len(), MAX_MEMORIES);
    assert_eq!(store.entries[0].id, 6);
    assert!(!store.strokes_path(1).exists());
    assert!(store.strokes_path(6).exists());
    let _ = std::fs::remove_dir_all(&store.dir);
}

#[test]
fn catalog_is_numbered_newest_first() {
    let mut store = tmp_store("catalog");
    store.append(1751856000, "about the garden", "…", &vec![vec![(1, 1, 1)]]);
    store.append(1751942400, "about the rain", "…", &vec![vec![(1, 1, 1)]]);
    let (lines, ids) = store.catalog(10);
    assert_eq!(ids, vec![1751942400, 1751856000]);
    assert!(lines[0].starts_with("1. "));
    assert!(lines[0].contains("about the rain"));
    assert!(lines[1].contains("about the garden"));
    let _ = std::fs::remove_dir_all(&store.dir);
}

#[test]
fn history_panel_deletes_newest_visible_entry_and_strokes() {
    let mut store = tmp_store("history-delete");
    store.append(101, "第一问", "第一答", &vec![vec![(1, 1, 1)]]);
    store.append(102, "第二问", "第二答", &vec![vec![(2, 2, 2)]]);
    assert!(store.panel_lines(9)[0].contains("第二问"));
    let removed = store.delete_number(1, 9).unwrap();
    assert_eq!(removed.id, 102);
    assert!(!store.strokes_path(102).exists());
    assert_eq!(store.entries.len(), 1);
    let mut reopened = MemoryStore {
        dir: store.dir.clone(),
        entries: Vec::new(),
    };
    reopened.load();
    assert_eq!(reopened.entries[0].id, 101);
    let _ = std::fs::remove_dir_all(&store.dir);
}

#[test]
fn ids_are_unique_within_one_wall_clock_second() {
    let mut store = tmp_store("ids");
    assert_eq!(store.next_id(42), 42);
    store.append(42, "a", "b", &Vec::new());
    assert_eq!(store.next_id(42), 43);
    let _ = std::fs::remove_dir_all(&store.dir);
}

#[test]
fn spoken_dates_read_like_a_diary() {
    // 2026-07-06 23:30 UTC.
    let date = spoken_date(1783467000);
    assert!(date.contains("of July"), "{date}");
    assert!(date.contains("6th") || date.contains("7th"), "{date}");
}
