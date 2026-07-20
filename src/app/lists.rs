//! Local font, task, TODO, and history panel actions.

use std::path::PathBuf;
use std::time::Instant;

use crate::surface::Surface;
use crate::{display, fonts, memory, reader, tasks, todos, ui};

use super::state::{State, TurnKind};
use super::timing::{heartbeat_deadline, unix_now};

pub(super) const HISTORY_VISIBLE: usize = 9;

pub(super) fn finish_paper_list_stroke(
    state: &mut State,
    memory_store: &mut Option<memory::MemoryStore>,
    task_store: &mut Option<tasks::TaskStore>,
    todo_store: &mut Option<todos::TodoStore>,
    next_heartbeat: &mut Option<Instant>,
    surf: &mut Surface,
    font: &mut fonts::FontBook,
    disp: &display::Display,
) -> Option<PathBuf> {
    if matches!(state, State::FontList { .. }) {
        let action = match state {
            State::FontList { panel } => panel.pen_up(),
            _ => None,
        };
        match action {
            Some(ui::font_settings::Action::Select(id)) => {
                if let Err(error) = font.select(id) {
                    eprintln!("magic-paper: could not persist font selection: {error}");
                }
                if let State::FontList { panel } = state {
                    panel.redraw(surf, font);
                }
                disp.update_all(surf.w, surf.h);
                eprintln!("magic-paper: selected font {}", id.stable_id());
            }
            Some(ui::font_settings::Action::SetScale(id, percent)) => {
                if let Err(error) = font.set_scale_percent(id, percent) {
                    eprintln!("magic-paper: could not persist font size calibration: {error}");
                }
                if let State::FontList { panel } = state {
                    panel.redraw(surf, font);
                }
                disp.update_all(surf.w, surf.h);
                eprintln!(
                    "magic-paper: calibrated font {} to {}%",
                    id.stable_id(),
                    font.scale_percent(id)
                );
            }
            Some(ui::font_settings::Action::Dismiss) => {
                let old = std::mem::replace(state, State::Listening { last_pen: None });
                match old {
                    State::FontList { panel } => panel.dismiss(surf),
                    _ => unreachable!(),
                }
                disp.update_all(surf.w, surf.h);
                eprintln!("magic-paper: font list dismissed");
            }
            Some(ui::font_settings::Action::Redraw) => {
                if let State::FontList { panel } = state {
                    panel.redraw(surf, font);
                }
                disp.update_all(surf.w, surf.h);
            }
            None => {}
        }
        return None;
    }

    if matches!(state, State::ReaderList { .. }) {
        let action = match state {
            State::ReaderList { panel, .. } => panel.pen_up(),
            _ => None,
        };
        match action {
            Some(ui::paper_list::Action::Select(number)) => {
                let path = match state {
                    State::ReaderList { books, .. } => books
                        .get(number.wrapping_sub(1))
                        .map(|book| book.path.clone()),
                    _ => None,
                };
                if let Some(path) = path {
                    eprintln!(
                        "magic-paper: reader candidate {number} selected — {}",
                        path.display()
                    );
                    return Some(path);
                }
                redraw_reader_list(state, surf, font);
                disp.update_all(surf.w, surf.h);
            }
            Some(ui::paper_list::Action::Dismiss) => {
                let old = std::mem::replace(state, State::Listening { last_pen: None });
                match old {
                    State::ReaderList { panel, .. } => panel.dismiss(surf),
                    _ => unreachable!(),
                }
                disp.update_all(surf.w, surf.h);
                eprintln!("magic-paper: reader candidates dismissed");
            }
            Some(ui::paper_list::Action::Redraw)
            | Some(ui::paper_list::Action::Delete(_))
            | Some(ui::paper_list::Action::Toggle(_)) => {
                redraw_reader_list(state, surf, font);
                disp.update_all(surf.w, surf.h);
            }
            None => {}
        }
        return None;
    }

    let action = match state {
        State::TaskList { panel } | State::TodoList { panel } | State::HistoryList { panel } => {
            panel.pen_up()
        }
        _ => None,
    };
    let Some(action) = action else {
        return None;
    };
    match action {
        ui::paper_list::Action::Delete(number) => {
            match state {
                State::TaskList { .. } => match task_store.as_mut() {
                    Some(store) => match store.delete_number(number) {
                        Ok(task) => {
                            eprintln!(
                                "magic-paper: task {number} deleted from paper list — {}",
                                task.instruction
                            );
                            *next_heartbeat = heartbeat_deadline(task_store);
                        }
                        Err(e) => eprintln!("magic-paper: could not delete task {number}: {e}"),
                    },
                    None => eprintln!("magic-paper: task storage is disabled"),
                },
                State::TodoList { .. } => match todo_store.as_mut() {
                    Some(store) => match store.delete_number(number) {
                        Ok(todo) => eprintln!(
                            "magic-paper: TODO {number} deleted from paper list — {}",
                            todo.text
                        ),
                        Err(e) => eprintln!("magic-paper: could not delete TODO {number}: {e}"),
                    },
                    None => eprintln!("magic-paper: TODO storage is disabled"),
                },
                State::HistoryList { .. } => match memory_store.as_mut() {
                    Some(store) => match store.delete_number(number, HISTORY_VISIBLE) {
                        Ok(entry) => eprintln!(
                            "magic-paper: history {number} deleted — {}",
                            entry.transcript
                        ),
                        Err(error) => {
                            eprintln!("magic-paper: could not delete history {number}: {error}")
                        }
                    },
                    None => eprintln!("magic-paper: memory storage is disabled"),
                },
                _ => {}
            }
            redraw_paper_list(state, memory_store, task_store, todo_store, surf, font);
            disp.update_all(surf.w, surf.h);
        }
        ui::paper_list::Action::Toggle(number) => {
            if let State::TaskList { .. } = state {
                match task_store.as_mut() {
                    Some(store) => match store.toggle_number(number, unix_now()) {
                        Ok(task) => {
                            eprintln!(
                                "magic-paper: task {number} {} from paper list",
                                if task.paused { "disabled" } else { "enabled" }
                            );
                            *next_heartbeat = heartbeat_deadline(task_store);
                        }
                        Err(error) => {
                            eprintln!("magic-paper: could not toggle task {number}: {error}")
                        }
                    },
                    None => eprintln!("magic-paper: task storage is disabled"),
                }
            }
            redraw_paper_list(state, memory_store, task_store, todo_store, surf, font);
            disp.update_all(surf.w, surf.h);
        }
        ui::paper_list::Action::Dismiss => {
            let old = std::mem::replace(state, State::Listening { last_pen: None });
            match old {
                State::TaskList { panel }
                | State::TodoList { panel }
                | State::HistoryList { panel } => panel.dismiss(surf),
                _ => unreachable!(),
            }
            disp.update_all(surf.w, surf.h);
            eprintln!("magic-paper: paper list dismissed");
        }
        ui::paper_list::Action::Redraw => {
            redraw_paper_list(state, memory_store, task_store, todo_store, surf, font);
            disp.update_all(surf.w, surf.h);
        }
        ui::paper_list::Action::Select(_) => {}
    }
    None
}

fn redraw_reader_list(state: &mut State, surf: &mut Surface, font: &fonts::FontBook) {
    if let State::ReaderList { panel, books } = state {
        let lines: Vec<String> = books.iter().map(reader::Book::panel_label).collect();
        panel.redraw(
            surf,
            font,
            "選擇要閱讀的書",
            "沒有相符書籍",
            "用筆點書名開啟 · 點空白退出",
            &lines,
            None,
        );
    }
}

fn redraw_paper_list(
    state: &mut State,
    memory_store: &Option<memory::MemoryStore>,
    task_store: &Option<tasks::TaskStore>,
    todo_store: &Option<todos::TodoStore>,
    surf: &mut Surface,
    font: &fonts::FontBook,
) {
    match state {
        State::TaskList { panel } => {
            let lines = task_store
                .as_ref()
                .map(|store| store.panel_lines())
                .unwrap_or_default();
            let enabled = task_store
                .as_ref()
                .map(|store| store.panel_enabled())
                .unwrap_or_default();
            panel.redraw(
                surf,
                font,
                "任務列表",
                "尚無任務",
                "橫劃可刪除 · 點右側方框啟用或停用 · 點空白退出",
                &lines,
                Some(&enabled),
            );
        }
        State::TodoList { panel } => {
            let lines = todo_store
                .as_ref()
                .map(|store| store.panel_lines())
                .unwrap_or_default();
            panel.redraw(
                surf,
                font,
                "TODO 列表",
                "尚無 TODO",
                "橫劃 TODO 可刪除 · 點擊空白處退出",
                &lines,
                None,
            );
        }
        State::HistoryList { panel } => {
            let lines = memory_store
                .as_ref()
                .map(|store| store.panel_lines(HISTORY_VISIBLE))
                .unwrap_or_default();
            panel.redraw(
                surf,
                font,
                "對話歷史",
                "尚無歷史",
                "橫劃一段歷史可刪除 · 點擊空白處退出",
                &lines,
                None,
            );
        }
        _ => {}
    }
}

pub(super) fn apply_local_command(
    command: &str,
    task_store: &mut Option<tasks::TaskStore>,
    todo_store: &mut Option<todos::TodoStore>,
) -> (String, bool) {
    if let Some(store) = task_store.as_mut() {
        match store.apply_from_transcript(command, unix_now()) {
            Ok(Some(change)) => {
                let reply = match change {
                    tasks::TaskChange::Added(task) => {
                        format!("任務已新增：{}。", task.instruction)
                    }
                    tasks::TaskChange::Deleted { number, task } => {
                        format!("已刪除任務 {number}：{}。", task.instruction)
                    }
                    tasks::TaskChange::Paused { number, .. } => {
                        format!("任務 {number} 已暫停。")
                    }
                    tasks::TaskChange::Resumed { number, .. } => {
                        format!("任務 {number} 已恢復。")
                    }
                    tasks::TaskChange::Modified { number, after, .. } => {
                        format!("任務 {number} 已修改為：{}。", after.instruction)
                    }
                };
                eprintln!("magic-paper: local task command applied — {command}");
                return (reply, true);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("magic-paper: local task command rejected: {error}");
                return (
                    "任務指令無法執行，請檢查編號、間隔或任務上限。".into(),
                    false,
                );
            }
        }
    }

    if let Some(store) = todo_store.as_mut() {
        match store.add_from_transcript(command, unix_now()) {
            Ok(Some(todo)) => {
                eprintln!("magic-paper: local TODO added — {}", todo.text);
                return (format!("TODO 已新增：{}。", todo.text), false);
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("magic-paper: local TODO rejected: {error}");
                return ("TODO 無法新增，請檢查內容或列表上限。".into(), false);
            }
        }
    }

    ("指令未能辨識，請再寫一次。".into(), false)
}

pub(super) fn accept_transcript(
    slot: &mut Option<String>,
    transcript: String,
    kind: TurnKind,
    task_store: &mut Option<tasks::TaskStore>,
    todo_store: &mut Option<todos::TodoStore>,
) -> bool {
    let first = slot.is_none();
    let mut scheduled_tasks_changed = false;
    if first && kind == TurnKind::User {
        if let Some(store) = task_store.as_mut() {
            match store.apply_from_transcript(&transcript, unix_now()) {
                Ok(Some(change)) => {
                    scheduled_tasks_changed = true;
                    eprintln!("magic-paper: task list changed — {change:?}");
                }
                Ok(None) => {}
                Err(e) => eprintln!("magic-paper: task command rejected: {e}"),
            }
        }
        if let Some(store) = todo_store.as_mut() {
            match store.add_from_transcript(&transcript, unix_now()) {
                Ok(Some(todo)) => eprintln!("magic-paper: TODO added — {}", todo.text),
                Ok(None) => {}
                Err(e) => eprintln!("magic-paper: TODO command rejected: {e}"),
            }
        }
    }
    *slot = Some(transcript);
    scheduled_tasks_changed
}
