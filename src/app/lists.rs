//! Local font, task, TODO, and history panel actions.

use std::path::PathBuf;
use std::time::Instant;

use crate::platform::RefreshIntent;
use crate::surface::Surface;
use crate::ui::pointer::Gesture;
use crate::{display, fonts, memory, reader, tasks, todos, ui};

use super::state::{State, TurnKind};
use super::timing::{heartbeat_deadline, unix_now};
use super::{refresh_controller::RefreshController, settings_controller};

pub(super) const HISTORY_VISIBLE: usize = 9;

pub(super) struct PaperListContext<'a> {
    pub memory_store: &'a mut Option<memory::MemoryStore>,
    pub task_store: &'a mut Option<tasks::TaskStore>,
    pub todo_store: &'a mut Option<todos::TodoStore>,
    pub next_heartbeat: &'a mut Option<Instant>,
    pub surf: &'a mut Surface,
    pub font: &'a mut fonts::FontBook,
    pub disp: &'a display::Display,
    pub refresh: &'a mut RefreshController,
}

pub(super) fn finish_paper_list_stroke(
    state: &mut State,
    context: PaperListContext<'_>,
    gesture: Gesture,
) -> Option<PathBuf> {
    let PaperListContext {
        memory_store,
        task_store,
        todo_store,
        next_heartbeat,
        surf,
        font,
        disp,
        refresh,
    } = context;
    if matches!(state, State::Settings { .. }) {
        settings_controller::finish_settings_stroke(state, surf, font, disp, refresh, gesture);
        return None;
    }
    if matches!(state, State::FontList { .. }) {
        finish_font_stroke(state, surf, font, disp, refresh, gesture);
        return None;
    }
    if matches!(state, State::ReaderList { .. }) {
        return finish_reader_stroke(state, surf, font, disp, gesture);
    }
    let mut stores = ListStores {
        memory: memory_store,
        tasks: task_store,
        todos: todo_store,
        next_heartbeat,
    };
    finish_stored_list_stroke(state, &mut stores, surf, font, disp, refresh, gesture);
    None
}

struct ListStores<'a> {
    memory: &'a mut Option<memory::MemoryStore>,
    tasks: &'a mut Option<tasks::TaskStore>,
    todos: &'a mut Option<todos::TodoStore>,
    next_heartbeat: &'a mut Option<Instant>,
}

fn finish_font_stroke(
    state: &mut State,
    surf: &mut Surface,
    font: &mut fonts::FontBook,
    disp: &display::Display,
    refresh: &RefreshController,
    gesture: Gesture,
) {
    let action = match state {
        State::FontList { panel, .. } => panel.interact(gesture),
        _ => return,
    };
    match action {
        Some(ui::font_settings::Action::Select(id)) => {
            if let Err(error) = font.select(id) {
                eprintln!("magic-paper: could not persist font selection: {error}");
            }
            redraw_font_list(state, surf, font, disp);
            eprintln!("magic-paper: selected font {}", id.stable_id());
        }
        Some(ui::font_settings::Action::SetScale(id, percent)) => {
            if let Err(error) = font.set_scale_percent(id, percent) {
                eprintln!("magic-paper: could not persist font size calibration: {error}");
            }
            redraw_font_list(state, surf, font, disp);
            eprintln!(
                "magic-paper: calibrated font {} to {}%",
                id.stable_id(),
                font.scale_percent(id)
            );
        }
        Some(ui::font_settings::Action::Dismiss) => {
            let old = std::mem::replace(state, State::Listening { last_pen: None });
            if let State::FontList { panel, origin } = old {
                panel.dismiss(surf);
                *state = match origin {
                    super::state::FontOrigin::Paper => State::Listening { last_pen: None },
                    super::state::FontOrigin::Settings(mut panel) => {
                        panel.redraw(surf, font, refresh.values());
                        State::Settings { panel }
                    }
                };
            }
            disp.present_all(surf.w, surf.h, RefreshIntent::Content);
            eprintln!("magic-paper: font list dismissed");
        }
        Some(ui::font_settings::Action::Redraw) => redraw_font_list(state, surf, font, disp),
        None => {}
    }
}

fn redraw_font_list(
    state: &mut State,
    surf: &mut Surface,
    font: &fonts::FontBook,
    disp: &display::Display,
) {
    if let State::FontList { panel, .. } = state {
        panel.redraw(surf, font);
    }
    disp.present_all(surf.w, surf.h, RefreshIntent::Content);
}

fn finish_reader_stroke(
    state: &mut State,
    surf: &mut Surface,
    font: &fonts::FontBook,
    disp: &display::Display,
    gesture: Gesture,
) -> Option<PathBuf> {
    let action = match state {
        State::ReaderList { panel, .. } => panel.interact(gesture),
        _ => return None,
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
            disp.present_all(surf.w, surf.h, RefreshIntent::Content);
        }
        Some(ui::paper_list::Action::Dismiss) => {
            let old = std::mem::replace(state, State::Listening { last_pen: None });
            if let State::ReaderList { panel, .. } = old {
                panel.dismiss(surf);
            }
            disp.present_all(surf.w, surf.h, RefreshIntent::Content);
            eprintln!("magic-paper: reader candidates dismissed");
        }
        Some(_) => {
            redraw_reader_list(state, surf, font);
            disp.present_all(surf.w, surf.h, RefreshIntent::Content);
        }
        None => {}
    }
    None
}

fn finish_stored_list_stroke(
    state: &mut State,
    stores: &mut ListStores<'_>,
    surf: &mut Surface,
    font: &fonts::FontBook,
    disp: &display::Display,
    refresh: &RefreshController,
    gesture: Gesture,
) {
    let action = match state {
        State::TaskList { panel } | State::TodoList { panel } | State::HistoryList { panel } => {
            panel.interact(gesture)
        }
        _ => None,
    };
    let Some(action) = action else { return };
    match action {
        ui::paper_list::Action::Delete(number) => {
            delete_stored_row(state, stores, number);
            redraw_stored_list(state, stores, surf, font, disp, refresh, true);
        }
        ui::paper_list::Action::Toggle(number) => {
            toggle_task(stores, number);
            redraw_stored_list(state, stores, surf, font, disp, refresh, false);
        }
        ui::paper_list::Action::Dismiss => {
            let old = std::mem::replace(state, State::Listening { last_pen: None });
            if let State::TaskList { panel }
            | State::TodoList { panel }
            | State::HistoryList { panel } = old
            {
                panel.dismiss(surf);
            }
            disp.present_all(surf.w, surf.h, RefreshIntent::Content);
            eprintln!("magic-paper: paper list dismissed");
        }
        ui::paper_list::Action::Redraw => {
            redraw_stored_list(state, stores, surf, font, disp, refresh, false)
        }
        ui::paper_list::Action::Select(_) => {}
    }
}

fn delete_stored_row(state: &State, stores: &mut ListStores<'_>, number: usize) {
    match state {
        State::TaskList { .. } => match stores.tasks.as_mut() {
            Some(store) => match store.delete_number(number) {
                Ok(task) => {
                    eprintln!(
                        "magic-paper: task {number} deleted from paper list — {}",
                        task.instruction
                    );
                    *stores.next_heartbeat = heartbeat_deadline(stores.tasks);
                }
                Err(error) => eprintln!("magic-paper: could not delete task {number}: {error}"),
            },
            None => eprintln!("magic-paper: task storage is disabled"),
        },
        State::TodoList { .. } => match stores.todos.as_mut() {
            Some(store) => match store.delete_number(number) {
                Ok(todo) => eprintln!(
                    "magic-paper: TODO {number} deleted from paper list — {}",
                    todo.text
                ),
                Err(error) => eprintln!("magic-paper: could not delete TODO {number}: {error}"),
            },
            None => eprintln!("magic-paper: TODO storage is disabled"),
        },
        State::HistoryList { .. } => match stores.memory.as_mut() {
            Some(store) => match store.delete_number(number, HISTORY_VISIBLE) {
                Ok(entry) => eprintln!(
                    "magic-paper: history {number} deleted — {}",
                    entry.transcript
                ),
                Err(error) => eprintln!("magic-paper: could not delete history {number}: {error}"),
            },
            None => eprintln!("magic-paper: memory storage is disabled"),
        },
        _ => {}
    }
}

fn toggle_task(stores: &mut ListStores<'_>, number: usize) {
    match stores.tasks.as_mut() {
        Some(store) => match store.toggle_number(number, unix_now()) {
            Ok(task) => {
                eprintln!(
                    "magic-paper: task {number} {} from paper list",
                    if task.paused { "disabled" } else { "enabled" }
                );
                *stores.next_heartbeat = heartbeat_deadline(stores.tasks);
            }
            Err(error) => eprintln!("magic-paper: could not toggle task {number}: {error}"),
        },
        None => eprintln!("magic-paper: task storage is disabled"),
    }
}

fn redraw_stored_list(
    state: &mut State,
    stores: &ListStores<'_>,
    surf: &mut Surface,
    font: &fonts::FontBook,
    disp: &display::Display,
    refresh: &RefreshController,
    cleanup: bool,
) {
    redraw_paper_list(state, stores.memory, stores.tasks, stores.todos, surf, font);
    if cleanup {
        let region = match state {
            State::TaskList { panel }
            | State::TodoList { panel }
            | State::HistoryList { panel } => panel.refresh_region(),
            _ => return,
        };
        refresh.present_cleanup(disp, region);
    } else {
        disp.present_all(surf.w, surf.h, RefreshIntent::Content);
    }
}

fn redraw_reader_list(state: &mut State, surf: &mut Surface, font: &fonts::FontBook) {
    if let State::ReaderList { panel, books } = state {
        let lines: Vec<String> = books.iter().map(reader::Book::panel_label).collect();
        panel.redraw(
            surf,
            font,
            ui::paper_list::Content {
                title: "选择要阅读的书",
                empty_text: "没有相符书籍",
                footer: "用笔点书名打开 · 点空白退出",
                entries: &lines,
                enabled: None,
            },
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
                ui::paper_list::Content {
                    title: "任务列表",
                    empty_text: "尚无任务",
                    footer: "横划可删除 · 点右侧方框启用或停用 · 点空白退出",
                    entries: &lines,
                    enabled: Some(&enabled),
                },
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
                ui::paper_list::Content {
                    title: "TODO 列表",
                    empty_text: "尚无 TODO",
                    footer: "横划 TODO 可删除 · 点击空白处退出",
                    entries: &lines,
                    enabled: None,
                },
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
                ui::paper_list::Content {
                    title: "对话历史",
                    empty_text: "尚无历史",
                    footer: "横划一段历史可删除 · 点击空白处退出",
                    entries: &lines,
                    enabled: None,
                },
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
