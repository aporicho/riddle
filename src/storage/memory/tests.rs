use super::*;

fn tmp_store(name: &str) -> MemoryStore {
    let dir =
        std::env::temp_dir().join(format!("magicpaper-mem-test-{}-{name}", std::process::id()));
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
    reopened.load().unwrap();
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
fn new_dialogue_session_survives_reopen_without_deleting_recallable_history() {
    let mut store = tmp_store("dialogue-boundary");
    store.append(100, "旧问题", "旧回答", &Vec::new());
    assert_eq!(store.recent_dialogue(20).len(), 1);
    assert_eq!(store.begin_dialogue_session().unwrap(), 100);
    assert!(store.recent_dialogue(20).is_empty());
    assert_eq!(store.catalog(20).1, vec![100]);
    assert_eq!(store.panel_lines(20).len(), 1);

    store.append(101, "新问题", "新回答", &Vec::new());
    let mut reopened = MemoryStore {
        dir: store.dir.clone(),
        entries: Vec::new(),
    };
    reopened.load().unwrap();
    assert_eq!(
        reopened.recent_dialogue(20),
        vec![("新问题".into(), "新回答".into())]
    );
    assert_eq!(reopened.catalog(20).1, vec![101, 100]);
    let _ = std::fs::remove_dir_all(&store.dir);
}

#[test]
fn corrupt_dialogue_boundary_fails_closed_instead_of_restoring_old_context() {
    let mut store = tmp_store("corrupt-dialogue-boundary");
    store.append(100, "不应泄露", "旧回答", &Vec::new());
    std::fs::write(store.dialogue_floor_path(), "not-a-number\n").unwrap();
    assert!(store.recent_dialogue(20).is_empty());
    assert_eq!(store.catalog(20).1, vec![100]);
    let _ = std::fs::remove_dir_all(&store.dir);
}

#[test]
fn recent_dialogue_is_bounded_without_splitting_utf8_or_dropping_short_turns() {
    let mut store = tmp_store("bounded-dialogue");
    for id in 1..=20 {
        store.append(id, "短问题", "短回答", &Vec::new());
    }
    assert_eq!(store.recent_dialogue(20).len(), 20);

    store.append(21, &"问".repeat(20_000), &"答".repeat(20_000), &Vec::new());
    let recent = store.recent_dialogue(1);
    assert_eq!(recent.len(), 1);
    assert!(recent[0].0.len() <= MAX_DIALOGUE_FIELD_BYTES);
    assert!(recent[0].1.len() <= MAX_DIALOGUE_FIELD_BYTES);
    assert!(recent[0].0.is_char_boundary(recent[0].0.len()));
    assert!(recent[0].1.is_char_boundary(recent[0].1.len()));
    let _ = std::fs::remove_dir_all(&store.dir);
}

#[test]
fn rejects_oversized_memory_fields_and_stroke_files() {
    let mut store = tmp_store("oversized-fields");
    let oversized = "x".repeat(MAX_MEMORY_FIELD_BYTES + 1);
    let error = store
        .try_append(100, &oversized, "回答", &Vec::new())
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(!store.index_path().exists());

    std::fs::write(store.index_path(), format!("100\t{oversized}\t回答\n")).unwrap();
    assert_eq!(
        store.load().unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );

    std::fs::write(store.strokes_path(100), vec![b'x'; MAX_STROKES_BYTES + 1]).unwrap();
    assert!(store.strokes(100).is_none());
    let _ = std::fs::remove_dir_all(store.dir);
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
    reopened.load().unwrap();
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

#[test]
fn corrupt_memory_index_is_reported_and_never_overwritten() {
    let mut store = tmp_store("corrupt-index");
    store.append(100, "第一问", "第一答", &Vec::new());
    let cached_id = store.entries[0].id;
    let corrupt = b"broken memory record\n";
    std::fs::write(store.index_path(), corrupt).unwrap();

    let error = store.load().unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(store.entries.len(), 1);
    assert_eq!(store.entries[0].id, cached_id);
    assert!(store
        .try_append(101, "第二问", "第二答", &Vec::new())
        .is_err());
    assert_eq!(std::fs::read(store.index_path()).unwrap(), corrupt);
    assert!(!store.strokes_path(101).exists());
    let _ = std::fs::remove_dir_all(store.dir);
}

#[test]
fn non_not_found_memory_read_error_is_not_an_empty_store() {
    let mut store = tmp_store("read-error");
    std::fs::create_dir(store.index_path()).unwrap();
    let error = store.load().unwrap_err();
    assert_ne!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(store.try_append(100, "问题", "回答", &Vec::new()).is_err());
    assert!(store.index_path().is_dir());
    let _ = std::fs::remove_dir_all(store.dir);
}

#[test]
fn failed_atomic_memory_index_replacement_keeps_old_index_and_strokes() {
    let mut store = tmp_store("atomic-failure");
    store
        .try_append(100, "第一问", "第一答", &vec![vec![(1, 1, 1)]])
        .unwrap();
    let before = std::fs::read(store.index_path()).unwrap();
    std::fs::create_dir(store.dir.join("index.tsv.new")).unwrap();

    assert!(store
        .try_append(101, "第二问", "第二答", &vec![vec![(2, 2, 2)]])
        .is_err());
    assert_eq!(std::fs::read(store.index_path()).unwrap(), before);
    assert!(store.strokes_path(100).exists());
    assert!(!store.strokes_path(101).exists());
    assert_eq!(store.entries.len(), 1);
    let _ = std::fs::remove_dir_all(store.dir);
}

#[test]
fn stroke_write_failure_cannot_publish_a_memory_index_entry() {
    let mut store = tmp_store("stroke-failure");
    std::fs::create_dir(store.dir.join("100.strokes.new")).unwrap();
    assert!(store
        .try_append(100, "问题", "回答", &vec![vec![(1, 1, 1)]])
        .is_err());
    assert!(!store.index_path().exists());
    assert!(store.entries.is_empty());
    let _ = std::fs::remove_dir_all(store.dir);
}

#[test]
fn concurrent_memory_appends_preserve_both_turns_and_allocate_unique_ids() {
    let original = tmp_store("concurrent-add");
    let dir = original.dir.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for transcript in ["第一问", "第二问"] {
        let dir = dir.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut store = MemoryStore {
                dir,
                entries: Vec::new(),
            };
            barrier.wait();
            store
                .try_append(100, transcript, "回答", &Vec::new())
                .unwrap()
        }));
    }
    barrier.wait();
    let ids = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert_ne!(ids[0], ids[1]);
    let mut reopened = MemoryStore {
        dir: dir.clone(),
        entries: Vec::new(),
    };
    reopened.load().unwrap();
    assert_eq!(reopened.entries.len(), 2);
    assert!(reopened
        .entries
        .iter()
        .all(|entry| reopened.strokes_path(entry.id).exists()));
    let _ = std::fs::remove_dir_all(dir);
}
