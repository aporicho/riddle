//! Routing for the first semantic event of an OCR/oracle turn.
//!
//! The runtime owns stores and surfaces, but it no longer embeds every oracle
//! directive branch. This controller consumes one typed event and returns the
//! next application state after executing the corresponding boundary effects.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::platform::RefreshIntent;
use crate::surface::Surface;
use crate::{display, fonts, memory, oracle::Event, reader, runtime_control, tasks, todos, ui};

use super::lists::{accept_transcript, apply_local_command, HISTORY_VISIBLE};
use super::oracle_controller::OracleTurn;
use super::reply::{conjure, oracle_excuse, plan_reply_async, plan_streaming_reply_async};
use super::state::{State, TurnKind};
use super::timing::heartbeat_deadline;

pub(super) struct FirstEventContext<'a> {
    pub(super) font: &'a fonts::FontBook,
    pub(super) memory_store: &'a Option<memory::MemoryStore>,
    pub(super) task_store: &'a mut Option<tasks::TaskStore>,
    pub(super) todo_store: &'a mut Option<todos::TodoStore>,
    pub(super) next_heartbeat: &'a mut Option<Instant>,
    pub(super) surface: &'a mut Surface,
    pub(super) display: &'a display::Display,
    pub(super) takeover: bool,
    pub(super) turn_transcript: &'a mut Option<String>,
    pub(super) turn_reply: &'a mut String,
    pub(super) turn_failed: &'a mut bool,
    pub(super) turn_kind: TurnKind,
}

pub(super) fn consume_first_event(
    result: Result<Event, String>,
    rx: OracleTurn,
    since: Instant,
    mut ctx: FirstEventContext<'_>,
) -> State {
    let event = match result {
        Ok(event) => event,
        Err(error) => return first_event_error(error, &rx, &mut ctx),
    };
    match event {
        Event::Show(id) => show_memory(id, &mut ctx),
        Event::TaskList => open_task_list(&mut ctx),
        Event::TodoList => open_todo_list(&mut ctx),
        Event::FontList => open_font_list(&mut ctx),
        Event::HistoryList => open_history_list(&mut ctx),
        Event::Help => open_help(&mut ctx),
        Event::Reader(query) => open_reader(query, &mut ctx),
        Event::FullRefresh => {
            ctx.display.request_refresh(ctx.surface.w, ctx.surface.h);
            State::Listening { last_pen: None }
        }
        Event::LocalCommand(command) => apply_command(command, &mut ctx),
        Event::Ink(text) => {
            ctx.turn_reply.push_str(&text);
            replying(ctx.font, &text, Some(rx))
        }
        Event::Transcript(transcript) => {
            if accept_transcript(
                ctx.turn_transcript,
                transcript,
                ctx.turn_kind,
                ctx.task_store,
                ctx.todo_store,
            ) {
                *ctx.next_heartbeat = heartbeat_deadline(ctx.task_store);
            }
            State::Thinking { rx, since }
        }
    }
}

fn first_event_error(error: String, rx: &OracleTurn, ctx: &mut FirstEventContext<'_>) -> State {
    eprintln!(
        "magic-paper: event=turn-error request={} stage=first-event error={:?}",
        rx.request_id(),
        error.lines().next().unwrap_or("unknown error")
    );
    eprintln!("riddle: oracle failed: {error}");
    *ctx.turn_failed = true;
    replying(ctx.font, &oracle_excuse(&error), None)
}

fn show_memory(id: u64, ctx: &mut FirstEventContext<'_>) -> State {
    match conjure(ctx.font, ctx.memory_store, id, ctx.surface, ctx.display) {
        Some(state) => state,
        None => {
            eprintln!("riddle: memory {id} is missing");
            *ctx.turn_failed = true;
            replying(ctx.font, &oracle_excuse("lost page"), None)
        }
    }
}

fn open_task_list(ctx: &mut FirstEventContext<'_>) -> State {
    let lines = ctx
        .task_store
        .as_ref()
        .map(|store| store.panel_lines())
        .unwrap_or_default();
    let enabled = ctx
        .task_store
        .as_ref()
        .map(|store| store.panel_enabled())
        .unwrap_or_default();
    let panel = ui::paper_list::PaperList::show(
        ctx.surface,
        ctx.font,
        "任务列表",
        "尚无任务",
        "横划可删除 · 点右侧方框启用或停用 · 点空白退出",
        &lines,
        Some(&enabled),
    );
    present_panel(ctx, "recurring-task");
    State::TaskList { panel }
}

fn open_todo_list(ctx: &mut FirstEventContext<'_>) -> State {
    let lines = ctx
        .todo_store
        .as_ref()
        .map(|store| store.panel_lines())
        .unwrap_or_default();
    let panel = ui::paper_list::PaperList::show(
        ctx.surface,
        ctx.font,
        "TODO 列表",
        "尚无 TODO",
        "横划 TODO 可删除 · 点击空白处退出",
        &lines,
        None,
    );
    present_panel(ctx, "TODO");
    State::TodoList { panel }
}

fn open_font_list(ctx: &mut FirstEventContext<'_>) -> State {
    let panel = ui::font_settings::FontPanel::show(ctx.surface, ctx.font);
    present_panel(ctx, "font");
    State::FontList { panel }
}

fn open_history_list(ctx: &mut FirstEventContext<'_>) -> State {
    let lines = ctx
        .memory_store
        .as_ref()
        .map(|store| store.panel_lines(HISTORY_VISIBLE))
        .unwrap_or_default();
    let panel = ui::paper_list::PaperList::show(
        ctx.surface,
        ctx.font,
        "对话历史",
        "尚无历史",
        "横划一段历史可删除 · 点击空白处退出",
        &lines,
        None,
    );
    present_panel(ctx, "history");
    State::HistoryList { panel }
}

fn present_panel(ctx: &FirstEventContext<'_>, name: &str) {
    ctx.display
        .present_all(ctx.surface.w, ctx.surface.h, RefreshIntent::Content);
    eprintln!("magic-paper: {name} list opened");
}

fn open_help(ctx: &mut FirstEventContext<'_>) -> State {
    let panel = ui::help::show(ctx.surface, ctx.font, ctx.takeover);
    let (x, y, width, height) = panel.region.rect();
    ctx.display
        .present_region(x, y, width, height, RefreshIntent::Ui);
    State::Help {
        panel: Some(panel),
        until: Instant::now() + Duration::from_secs(180),
    }
}

fn open_reader(query: Option<String>, ctx: &mut FirstEventContext<'_>) -> State {
    let catalog = match reader::Catalog::open() {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!("magic-paper: could not scan reader catalog: {error}");
            return replying(ctx.font, "書庫暫時無法讀取。", None);
        }
    };
    match catalog.lookup(query.as_deref()) {
        reader::Lookup::Open(path) => request_reader(ctx.font, &path),
        reader::Lookup::Choose(books) => show_reader_choices(books, ctx),
        reader::Lookup::Missing => {
            let text = match query {
                Some(title) => format!("沒有找到《{}》，請寫更完整的書名。", title),
                None => "沒有找到可用的 KOReader 書庫。".into(),
            };
            replying(ctx.font, &text, None)
        }
    }
}

fn show_reader_choices(books: Vec<reader::Book>, ctx: &mut FirstEventContext<'_>) -> State {
    let lines: Vec<String> = books.iter().map(reader::Book::panel_label).collect();
    let panel = ui::paper_list::PaperList::show_selectable(
        ctx.surface,
        ctx.font,
        "选择要阅读的书",
        "没有相符书籍",
        "用笔点书名打开 · 点空白退出",
        &lines,
    );
    ctx.display
        .present_all(ctx.surface.w, ctx.surface.h, RefreshIntent::Content);
    State::ReaderList { panel, books }
}

fn apply_command(command: String, ctx: &mut FirstEventContext<'_>) -> State {
    *ctx.turn_transcript = Some(command.clone());
    let (reply, tasks_changed) = apply_local_command(&command, ctx.task_store, ctx.todo_store);
    if tasks_changed {
        *ctx.next_heartbeat = heartbeat_deadline(ctx.task_store);
    }
    ctx.turn_reply.push_str(&reply);
    replying(ctx.font, &reply, None)
}

pub(super) fn request_reader(font: &fonts::FontBook, path: &Path) -> State {
    let target = match reader::validated_target(path) {
        Ok(target) => target,
        Err(error) => {
            eprintln!("magic-paper: rejected KOReader target: {error}");
            return replying(font, "無法開啟：書籍路徑不在可用書庫中。", None);
        }
    };
    match runtime_control::open_reader(&target) {
        Ok(()) => {
            eprintln!(
                "magic-paper: runtime accepted KOReader request — {}",
                target.display()
            );
            State::Listening { last_pen: None }
        }
        Err(error) => {
            eprintln!("magic-paper: KOReader runtime request failed: {error}");
            let reason = match error.kind() {
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                    "應用管理器控制服務尚未運行"
                }
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => {
                    "應用管理器響應超時"
                }
                std::io::ErrorKind::PermissionDenied => "應用管理器控制通道權限錯誤",
                _ => "應用管理器拒絕了啟動請求",
            };
            replying(
                font,
                &format!("無法開啟閱讀器：{reason}。請返回管理器後重試。"),
                None,
            )
        }
    }
}

fn replying(font: &fonts::FontBook, text: &str, rx: Option<OracleTurn>) -> State {
    let plan = if rx.is_some() {
        plan_streaming_reply_async(font, text)
    } else {
        plan_reply_async(font, text, None)
    };
    State::Replying {
        plan,
        next: Instant::now(),
        rx,
        page_full: false,
    }
}
