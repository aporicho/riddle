use super::*;

fn tmp_store(name: &str) -> TaskStore {
    let dir = std::env::temp_dir().join(format!(
        "magic-paper-task-test-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    TaskStore {
        dir,
        entries: Vec::new(),
    }
}

fn add(store: &mut TaskStore, text: &str, now: u64) -> Task {
    match store.apply_from_transcript(text, now).unwrap().unwrap() {
        TaskChange::Added(task) => task,
        other => panic!("expected addition, got {other:?}"),
    }
}

fn reopen(dir: &std::path::Path) -> TaskStore {
    let mut store = TaskStore {
        dir: dir.to_path_buf(),
        entries: Vec::new(),
    };
    store.load();
    store
}

#[test]
fn parses_simplified_and_traditional_task_commands() {
    assert_eq!(
        parse_command("任务 每五分钟讲一个黑暗冷笑话").unwrap(),
        Some(TaskCommand::Add {
            interval_secs: 300,
            instruction: "讲一个黑暗冷笑话".into(),
        })
    );
    assert_eq!(
        parse_command("任務：每 10 分鐘 說一句哲學語錄").unwrap(),
        Some(TaskCommand::Add {
            interval_secs: 600,
            instruction: "說一句哲學語錄".into(),
        })
    );
}

#[test]
fn parses_larger_chinese_intervals() {
    assert_eq!(
        parse_command("任务 每二十五分钟提醒我喝水").unwrap(),
        Some(TaskCommand::Add {
            interval_secs: 1500,
            instruction: "提醒我喝水".into(),
        })
    );
    assert_eq!(
        parse_command("task 每两小时回顾目标").unwrap(),
        Some(TaskCommand::Add {
            interval_secs: 7200,
            instruction: "回顾目标".into(),
        })
    );
}

#[test]
fn parses_management_commands_and_task_numbers() {
    assert_eq!(
        parse_command("删除任务 2").unwrap(),
        Some(TaskCommand::Delete(2))
    );
    assert_eq!(
        parse_command("任務：暫停第二項").unwrap(),
        Some(TaskCommand::Pause(2))
    );
    assert_eq!(
        parse_command("恢复任务二号。 ").unwrap(),
        Some(TaskCommand::Resume(2))
    );
    assert_eq!(
        parse_command("修改任務 2 為 每十分鐘提醒我喝水").unwrap(),
        Some(TaskCommand::Modify {
            number: 2,
            interval_secs: 600,
            instruction: "提醒我喝水".into(),
        })
    );
}

#[test]
fn rejects_short_or_incomplete_tasks() {
    assert!(parse_command("任务 每一分钟响一次").is_err());
    assert!(parse_command("任务 每五分钟").is_err());
    assert!(parse_command("删除任务").is_err());
    assert!(parse_command("修改任务 1 提醒我").is_err());
    assert_eq!(parse_command("今天写点什么").unwrap(), None);
}

#[test]
fn persists_due_and_successful_run_state() {
    let mut s = tmp_store("round-trip");
    let task = add(&mut s, "任务 每五分钟讲一个黑暗冷笑话", 1000);
    assert!(s.due(1299).is_empty());
    assert_eq!(s.due(1300), vec![task.clone()]);
    assert!(s
        .complete_due_if_unchanged(std::slice::from_ref(&task), 1301)
        .unwrap());
    assert!(s.due(1600).is_empty());
    assert_eq!(s.due(1601).len(), 1);

    let dir = s.dir.clone();
    let mut reopened = TaskStore {
        dir: dir.clone(),
        entries: Vec::new(),
    };
    reopened.load();
    assert_eq!(reopened.entries.len(), 1);
    assert_eq!(reopened.entries[0].instruction, "讲一个黑暗冷笑话");
    assert_eq!(reopened.entries[0].next_due, 1601);
    assert!(!reopened.entries[0].paused);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn pauses_resumes_modifies_and_deletes_persistently() {
    let mut s = tmp_store("manage");
    add(&mut s, "任务 每五分钟讲笑话", 1000);
    add(&mut s, "任务 每十分钟提醒喝水", 1001);

    let change = s
        .apply_from_transcript("暂停任务 1", 1100)
        .unwrap()
        .unwrap();
    assert!(matches!(change, TaskChange::Paused { number: 1, .. }));
    assert!(s.due(2000).iter().all(|task| task.instruction != "讲笑话"));

    s.apply_from_transcript("修改任务 1 每十五分钟讲冷笑话", 1200)
        .unwrap();
    assert!(s.entries[0].paused);
    assert_eq!(s.entries[0].interval_secs, 900);
    assert_eq!(s.entries[0].instruction, "讲冷笑话");

    s.apply_from_transcript("恢复任务 1", 1300).unwrap();
    assert!(!s.entries[0].paused);
    assert_eq!(s.entries[0].next_due, 2200);
    assert!(s
        .due(2199)
        .iter()
        .all(|task| task.instruction != "讲冷笑话"));

    let deleted = s
        .apply_from_transcript("删除任务 1", 1400)
        .unwrap()
        .unwrap();
    assert!(matches!(deleted, TaskChange::Deleted { number: 1, .. }));
    assert_eq!(s.entries.len(), 1);
    assert_eq!(
        s.catalog_lines()[0],
        "1. [active] every 10 minutes — 提醒喝水"
    );

    let dir = s.dir.clone();
    let mut reopened = TaskStore {
        dir: dir.clone(),
        entries: Vec::new(),
    };
    reopened.load();
    assert_eq!(reopened.entries, s.entries);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn paper_checkbox_toggles_and_restarts_the_interval() {
    let mut s = tmp_store("paper-toggle");
    add(&mut s, "任务 每五分钟提醒喝水", 1000);
    let disabled = s.toggle_number(1, 1100).unwrap();
    assert!(disabled.paused);
    assert_eq!(s.panel_enabled(), vec![false]);
    let enabled = s.toggle_number(1, 5000).unwrap();
    assert!(!enabled.paused);
    assert_eq!(enabled.next_due, 5300);
    assert_eq!(s.panel_enabled(), vec![true]);
    let _ = std::fs::remove_dir_all(s.dir);
}

#[test]
fn migrates_old_active_tasks_and_limits_the_list_to_nine() {
    let mut s = tmp_store("migration-and-limit");
    std::fs::write(s.index_path(), "7\t300\t900\t旧任务\\n一行\n").unwrap();
    s.load();
    assert_eq!(s.entries.len(), 1);
    assert!(!s.entries[0].paused);
    assert_eq!(s.entries[0].instruction, "旧任务\n一行");

    for n in 2..=MAX_TASKS {
        add(&mut s, &format!("任务 每五分钟任务{n}"), 1000 + n as u64);
    }
    assert_eq!(s.entries.len(), MAX_TASKS);
    assert!(s
        .apply_from_transcript("任务 每五分钟第十个任务", 2000)
        .unwrap_err()
        .contains("at most 9"));
    let _ = std::fs::remove_dir_all(s.dir);
}

#[test]
fn stale_agent_completion_cannot_resurrect_ui_changes() {
    let mut ui = tmp_store("agent-ui-race");
    let task = add(&mut ui, "任务 每五分钟提醒喝水", 1000);
    let dir = ui.dir.clone();
    let mut agent = reopen(&dir);
    let due_snapshot = vec![task.clone()];

    ui.apply_from_transcript("修改任务 1 每十分钟提醒休息", 1300)
        .unwrap();
    assert!(!agent
        .complete_due_if_unchanged(&due_snapshot, 1400)
        .unwrap());
    let current = reopen(&dir);
    assert_eq!(current.entries[0].instruction, "提醒休息");
    assert_eq!(current.entries[0].interval_secs, 600);
    assert_eq!(current.entries[0].next_due, 1900);

    let modified = current.entries[0].clone();
    ui.delete_number(1).unwrap();
    assert!(!agent.complete_due_if_unchanged(&[modified], 2000).unwrap());
    assert!(reopen(&dir).entries.is_empty());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn paused_task_is_not_advanced_by_stale_agent_store() {
    let mut ui = tmp_store("pause-race");
    let task = add(&mut ui, "任务 每五分钟提醒喝水", 1000);
    let dir = ui.dir.clone();
    let mut stale_agent = reopen(&dir);
    ui.toggle_number(1, 1200).unwrap();
    assert!(!stale_agent
        .complete_due_if_unchanged(std::slice::from_ref(&task), 1300)
        .unwrap());
    let current = reopen(&dir);
    assert!(current.entries[0].paused);
    assert_eq!(current.entries[0].next_due, task.next_due);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn only_one_concurrent_scheduler_can_complete_a_due_snapshot() {
    let mut original = tmp_store("double-complete");
    let task = add(&mut original, "任务 每五分钟提醒喝水", 1000);
    let dir = original.dir.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for now in [1301, 1302] {
        let dir = dir.clone();
        let task = task.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut store = reopen(&dir);
            barrier.wait();
            store.complete_due_if_unchanged(&[task], now).unwrap()
        }));
    }
    barrier.wait();
    let results: Vec<bool> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(results.iter().filter(|result| **result).count(), 1);
    assert_eq!(reopen(&dir).entries.len(), 1);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn concurrent_additions_reload_under_lock_and_keep_unique_ids() {
    let original = tmp_store("concurrent-add");
    let dir = original.dir.clone();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut handles = Vec::new();
    for (text, now) in [
        ("任务 每五分钟提醒喝水", 1000),
        ("任务 每十分钟提醒休息", 1001),
    ] {
        let dir = dir.clone();
        let barrier = std::sync::Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let mut store = reopen(&dir);
            barrier.wait();
            store.apply_from_transcript(text, now).unwrap();
        }));
    }
    barrier.wait();
    for handle in handles {
        handle.join().unwrap();
    }
    let current = reopen(&dir);
    assert_eq!(current.entries.len(), 2);
    assert_ne!(current.entries[0].id, current.entries[1].id);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn scheduler_lease_excludes_every_other_ui_or_agent_owner() {
    let store = tmp_store("scheduler-lease");
    let dir = store.dir.clone();
    drop(store);
    let owner = acquire_scheduler_lease_in(&dir).expect("first scheduler owns the lease");
    assert!(external_scheduler_active_in(&dir));
    let error = match acquire_scheduler_lease_in(&dir) {
        Ok(_) => panic!("a second scheduler acquired the same lease"),
        Err(error) => error,
    };
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    drop(owner);
    assert!(!external_scheduler_active_in(&dir));
    assert!(acquire_scheduler_lease_in(&dir).is_ok());
    let _ = std::fs::remove_dir_all(dir);
}
