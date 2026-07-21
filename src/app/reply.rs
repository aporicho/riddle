//! Reply layout, remembered-page conjuring, and paper-safe error messages.

use std::time::Instant;

use crate::fb::{screen_h, screen_w, BBox};
use crate::platform::RefreshIntent;
use crate::surface::{Surface, WHITE};
use crate::{display, fonts, memory, script};

use super::layout_controller::{LayoutJob, LayoutPoll};
use super::state::{ConjurePlan, State, WritePlan};

const REPLY_PX: f32 = 96.0;
const MARGIN_X: i32 = 120;

pub(super) fn region_all_white(surf: &Surface, region: BBox) -> bool {
    if region.is_empty() {
        return true;
    }
    for y in region.y0..=region.y1 {
        for x in region.x0..=region.x1 {
            if surf.luma(x, y) < 200 {
                return false;
            }
        }
    }
    true
}

/// What MP writes when the spirit cannot answer: short and actionable. The
/// raw error still goes to stderr.
pub(super) fn oracle_excuse(e: &str) -> String {
    if e.contains("no oracle") {
        "MagicPaper lies dormant: it found no oracle. \
         Put an API key in oracle.env, then open me again."
            .into()
    } else if e.starts_with("http 401") || e.starts_with("http 403") {
        "The oracle refused MagicPaper's key. Check RIDDLE_OPENAI_KEY in oracle.env.".into()
    } else if e.starts_with("http ") {
        let code = e.split(':').next().unwrap_or("an error");
        format!("The oracle rejected MagicPaper's plea ({code}). Check the model and endpoint in oracle.env.")
    } else if e.contains("request failed") || e.contains("timed out") {
        "MagicPaper cannot reach its oracle. Is the tablet connected to Wi-Fi?".into()
    } else if e.contains("empty reply") {
        "The spirit read your words but said nothing. Write again.".into()
    } else {
        "The ink blurred before it could answer. Write again.".into()
    }
}

/// Summon a remembered page: snapshot today's page, clear the paper, and plan
/// the memory's rewriting — the date in a small hand, the writer's own strokes
/// exactly as they were penned, MP's old reply beneath — all in faded ink.
pub(super) fn conjure(
    font: &fonts::FontBook,
    store: &Option<memory::MemoryStore>,
    id: u64,
    surf: &mut Surface,
    disp: &display::Display,
) -> Option<State> {
    let s = store.as_ref()?;
    let entry = s.get(id)?.clone();
    let strokes = s.strokes(id).unwrap_or_default();
    eprintln!(
        "riddle: conjuring memory {id} ({})",
        memory::spoken_date(id)
    );

    let saved = surf.copy_rect(0, 0, screen_w(), screen_h());
    surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
    disp.present_all(surf.w, surf.h, RefreshIntent::Content);

    let mut all: Vec<Vec<(i32, i32, i32)>> = Vec::new();
    let mut region = BBox::empty();

    // The date, small and centered near the top, like a diary heading.
    let date = memory::spoken_date(entry.id);
    let mut raster = script::rasterize_ui_line(font, &date, 54.0);
    script::thin(&mut raster);
    let x0 = (screen_w() as i32 - raster.width as i32) / 2;
    let mut ink_bottom = 64;
    for stroke in script::trace(&raster) {
        let mapped: Vec<(i32, i32, i32)> = stroke
            .iter()
            .map(|&(sx, sy)| (x0 + sx, 64 + sy, 1))
            .collect();
        for &(x, y, r) in &mapped {
            region.add(x, y, r + 2);
            ink_bottom = ink_bottom.max(y);
        }
        all.push(mapped);
    }

    // The writer's own hand, exactly as it was penned.
    for stroke in &strokes {
        for &(x, y, r) in stroke {
            region.add(x, y, r + 2);
            ink_bottom = ink_bottom.max(y);
        }
        all.push(stroke.clone());
    }

    // MP's old reply, below.
    if !entry.reply.is_empty() {
        let y = (ink_bottom + 130).min(screen_h() as i32 - 400);
        let reply = plan_reply(font, &entry.reply, Some(y));
        for stroke in reply.strokes {
            let mapped: Vec<(i32, i32, i32)> = stroke.iter().map(|&(x, y)| (x, y, 2)).collect();
            for &(x, y, r) in &mapped {
                region.add(x, y, r + 2);
            }
            all.push(mapped);
        }
    }

    Some(State::Conjuring {
        plan: ConjurePlan {
            strokes: all,
            stroke_i: 0,
            point_i: 0,
            region,
        },
        next: Instant::now(),
        saved,
    })
}

/// Lay out reply text and produce screen-space strokes. `y_start` continues a
/// streamed reply below its previous chunk; None places the first chunk.
pub(super) fn plan_reply(font: &fonts::FontBook, text: &str, y_start: Option<i32>) -> WritePlan {
    let max_w = (screen_w() as i32 - 2 * MARGIN_X) as f32;
    let lines = script::wrap(font, text, REPLY_PX, max_w);
    let line_h = (REPLY_PX * 1.25) as i32;
    let total_h = line_h * lines.len() as i32;
    let mut y = y_start.unwrap_or(((screen_h() as i32 - total_h) / 3).max(60));
    let mut strokes = Vec::new();
    let mut region = BBox::empty();
    let mut seed = 0x1234u32;
    let mut jitter = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        ((seed >> 16) % 7) as i32 - 3
    };

    for line_text in &lines {
        let mut raster = script::rasterize_line(font, line_text, REPLY_PX);
        script::thin(&mut raster);
        let line_strokes = script::trace(&raster);
        let x0 = (screen_w() as i32 - raster.width as i32) / 2;
        let wobble = jitter();
        for s in line_strokes {
            let mapped: Vec<(i32, i32)> = s
                .iter()
                .map(|&(sx, sy)| (x0 + sx, y + sy + wobble))
                .collect();
            for &(x, yy) in &mapped {
                region.add(x, yy, 5);
            }
            strokes.push(mapped);
        }
        y += line_h;
    }

    WritePlan {
        strokes,
        stroke_i: 0,
        point_i: 0,
        region,
        next_y: y,
        layout: None,
        queued_text: String::new(),
        layout_font: None,
    }
}

/// Start a visible reply without rasterizing fonts on the event-loop thread.
pub(super) fn plan_reply_async(
    font: &fonts::FontBook,
    text: &str,
    y_start: Option<i32>,
) -> WritePlan {
    WritePlan {
        strokes: Vec::new(),
        stroke_i: 0,
        point_i: 0,
        region: BBox::empty(),
        next_y: y_start.unwrap_or(0),
        layout: Some(LayoutJob::spawn(font.clone(), text.to_owned(), y_start)),
        queued_text: String::new(),
        layout_font: Some(font.clone()),
    }
}

/// Splice a streamed continuation chunk into a running write animation.
pub(super) fn append_reply(font: &fonts::FontBook, plan: &mut WritePlan, more: &str) {
    if more.trim().is_empty() {
        return;
    }
    if plan.layout_font.is_none() {
        plan.layout_font = Some(font.clone());
    }
    if plan.layout.is_some() {
        if !plan.queued_text.is_empty() {
            plan.queued_text.push(' ');
        }
        plan.queued_text.push_str(more);
    } else {
        plan.layout = Some(LayoutJob::spawn(
            font.clone(),
            more.to_owned(),
            Some(plan.next_y),
        ));
    }
}

/// Merge a completed worker result and immediately schedule the queued tail.
pub(super) fn poll_reply_layout(plan: &mut WritePlan) {
    let poll = match plan.layout.as_ref() {
        Some(job) => job.poll(),
        None => return,
    };
    match poll {
        LayoutPoll::Pending => return,
        LayoutPoll::Ready(result) => {
            plan.layout = None;
            if !result.region.is_empty() {
                plan.region.add(result.region.x0, result.region.y0, 0);
                plan.region.add(result.region.x1, result.region.y1, 0);
            }
            plan.strokes.extend(result.strokes);
            plan.next_y = result.next_y;
        }
        LayoutPoll::Failed => {
            eprintln!("magic-paper: reply layout worker exited without a result");
            plan.layout = None;
        }
    }
    if plan.layout.is_none() && !plan.queued_text.is_empty() {
        let text = std::mem::take(&mut plan.queued_text);
        if let Some(font) = plan.layout_font.clone() {
            plan.layout = Some(LayoutJob::spawn(font, text, Some(plan.next_y)));
        }
    }
}
