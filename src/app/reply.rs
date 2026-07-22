//! Reply layout, remembered-page conjuring, and paper-safe error messages.

use std::time::Instant;

use crate::fb::{screen_h, screen_w, BBox};
use crate::platform::RefreshIntent;
use crate::surface::{Surface, WHITE};
use crate::{display, fonts, memory, script};

use super::layout_controller::{LayoutJob, LayoutPoll};
use super::state::{ConjurePlan, State, WritePlan};

mod layout;

pub(super) use layout::{append_overflow_marker, plan_reply};
#[cfg(test)]
use layout::{visible_grapheme_count, MARGIN_X, MARGIN_Y};

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

/// Start a visible reply without rasterizing fonts on the event-loop thread.
pub(super) fn plan_reply_async(
    font: &fonts::FontBook,
    text: &str,
    y_start: Option<i32>,
) -> WritePlan {
    WritePlan {
        created_at: Instant::now(),
        first_damage_logged: false,
        strokes: Vec::new(),
        stroke_i: 0,
        point_i: 0,
        region: BBox::empty(),
        next_y: y_start.unwrap_or(0),
        layout: Some(LayoutJob::spawn(font.clone(), text.to_owned(), y_start)),
        queued_text: String::new(),
        layout_font: Some(font.clone()),
        initial_buffering: false,
        visible_graphemes: 0,
        truncated: false,
    }
}

/// Start a network-streamed reply. Keep its text off-panel until the complete
/// answer is known: a block cannot be vertically centered while its final
/// height is still changing. Rasterization and handwriting animation still
/// begin immediately when the stream closes.
pub(super) fn plan_streaming_reply_async(font: &fonts::FontBook, text: &str) -> WritePlan {
    let created_at = Instant::now();
    WritePlan {
        created_at,
        first_damage_logged: false,
        strokes: Vec::new(),
        stroke_i: 0,
        point_i: 0,
        region: BBox::empty(),
        next_y: 0,
        layout: None,
        queued_text: text.to_owned(),
        layout_font: Some(font.clone()),
        initial_buffering: true,
        visible_graphemes: 0,
        truncated: false,
    }
}

/// Replace an undrawn provisional streaming layout with one complete centered
/// layout. The old worker is cancellation-barriered by `LayoutJob::drop`; the
/// original timestamp is retained so first-ink telemetry still covers the
/// whole local pipeline rather than restarting at reflow time.
pub(super) fn recenter_undrawn_reply(
    font: &fonts::FontBook,
    plan: &mut WritePlan,
    complete_text: &str,
) -> bool {
    if plan.first_damage_logged || complete_text.trim().is_empty() {
        return false;
    }
    let created_at = plan.created_at;
    let mut centered = plan_reply_async(font, complete_text, None);
    centered.created_at = created_at;
    *plan = centered;
    eprintln!(
        "magic-paper: event=reply-layout-restart mode=centered-before-first-damage chars={} latency_ms={}",
        complete_text.chars().count(),
        created_at.elapsed().as_millis(),
    );
    true
}

/// Splice a streamed continuation chunk into a running write animation.
pub(super) fn append_reply(font: &fonts::FontBook, plan: &mut WritePlan, more: &str) {
    if more.trim().is_empty() {
        return;
    }
    if plan.layout_font.is_none() {
        plan.layout_font = Some(font.clone());
    }
    if plan.initial_buffering || plan.layout.is_some() {
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

/// Decide the initial placement without blocking. An open stream remains
/// buffered so its final block can be centered on both axes.
pub(super) fn start_initial_reply_layout(plan: &mut WritePlan, stream_open: bool) {
    if !plan.initial_buffering {
        return;
    }
    if stream_open {
        return;
    }
    plan.initial_buffering = false;
    let text = std::mem::take(&mut plan.queued_text);
    let Some(font) = plan.layout_font.clone() else {
        return;
    };
    if text.trim().is_empty() {
        return;
    }
    eprintln!(
        "magic-paper: event=reply-layout-start mode=centered-complete buffered_chars={} latency_ms={}",
        text.chars().count(),
        plan.created_at.elapsed().as_millis(),
    );
    plan.layout = Some(LayoutJob::spawn(font, text, None));
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
            plan.visible_graphemes = plan
                .visible_graphemes
                .saturating_add(result.visible_graphemes);
            plan.truncated |= result.truncated;
        }
        LayoutPoll::Failed => {
            eprintln!("magic-paper: reply layout worker exited without a result");
            plan.layout = None;
        }
    }
    if plan.truncated {
        plan.queued_text.clear();
    } else if !plan.initial_buffering && plan.layout.is_none() && !plan.queued_text.is_empty() {
        let text = std::mem::take(&mut plan.queued_text);
        if let Some(font) = plan.layout_font.clone() {
            plan.layout = Some(LayoutJob::spawn(font, text, Some(plan.next_y)));
        }
    }
}

#[cfg(test)]
mod tests {
    use ab_glyph::FontRef;
    use std::time::{Duration, Instant};

    use super::{
        append_overflow_marker, append_reply, plan_reply, plan_streaming_reply_async,
        poll_reply_layout, recenter_undrawn_reply, start_initial_reply_layout,
        visible_grapheme_count, MARGIN_X, MARGIN_Y,
    };
    use crate::fonts::{FontBook, FontId};

    fn font() -> FontBook {
        FontBook::for_test(
            FontRef::try_from_slice(include_bytes!("../../fonts/ChenYuluoyan-2.0-Thin.ttf"))
                .unwrap(),
            None,
        )
    }

    fn wait_for_layout(plan: &mut super::WritePlan) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while plan.layout.is_some() && Instant::now() < deadline {
            poll_reply_layout(plan);
            std::thread::yield_now();
        }
        assert!(plan.layout.is_none(), "reply layout worker did not finish");
    }

    #[test]
    fn visible_count_uses_unicode_graphemes_not_scalars_or_whitespace() {
        assert_eq!(visible_grapheme_count("a\u{301}"), 1);
        assert_eq!(visible_grapheme_count("👨‍👩‍👧‍👦"), 1);
        assert_eq!(visible_grapheme_count("你 好\n呀"), 3);
    }

    #[test]
    fn a_short_reply_is_centered_as_one_measured_block() {
        crate::fb::test_init_screen();
        let plan = plan_reply(&font(), "回答會整體置中。", None);
        assert!(!plan.region.is_empty());
        let middle = (plan.region.y0 + plan.region.y1) / 2;
        assert!((middle - crate::fb::screen_h() as i32 / 2).abs() < 100);
    }

    #[test]
    fn completed_single_chunk_stream_matches_direct_centered_layout() {
        crate::fb::test_init_screen();
        let font = font();
        let text = "單段回答也應整體置中。";
        let direct = plan_reply(&font, text, None);
        let mut streamed = plan_streaming_reply_async(&font, text);
        start_initial_reply_layout(&mut streamed, false);
        wait_for_layout(&mut streamed);
        assert_eq!(streamed.strokes, direct.strokes);
        assert_eq!(streamed.region.rect(), direct.region.rect());
        assert_eq!(streamed.visible_graphemes, direct.visible_graphemes);
    }

    #[test]
    fn calibrated_long_reply_stays_inside_the_move_content_rect() {
        crate::fb::test_init_screen();
        let mut font = font();
        font.set_scale_for_test(FontId::ChenYuluoyan, 180);
        let text = "這是一段用來驗證自適應排版的較長回答，應依照實際字體高度縮小字號並正確換行。"
            .repeat(5);
        let plan = plan_reply(&font, &text, None);
        assert!(!plan.strokes.is_empty());
        assert!(plan.next_y <= crate::fb::screen_h() as i32 - MARGIN_Y + 1);
        for &(x, y) in plan.strokes.iter().flatten() {
            assert!(x >= MARGIN_X && x < crate::fb::screen_w() as i32 - MARGIN_X);
            assert!(y >= MARGIN_Y - 3 && y < crate::fb::screen_h() as i32 - MARGIN_Y + 3);
        }
    }

    #[test]
    fn fast_multi_chunk_stream_is_measured_and_centered_as_one_block() {
        crate::fb::test_init_screen();
        let font = font();
        let first = "第一句。";
        let second = "第二句也一起置中。";
        let mut plan = plan_streaming_reply_async(&font, first);
        append_reply(&font, &mut plan, second);
        start_initial_reply_layout(&mut plan, false);
        wait_for_layout(&mut plan);
        let direct = plan_reply(&font, &format!("{first} {second}"), None);
        assert_eq!(
            plan.visible_graphemes,
            visible_grapheme_count(&format!("{first} {second}"))
        );
        assert_eq!(plan.strokes, direct.strokes);
        let middle = (plan.region.y0 + plan.region.y1) / 2;
        assert!((middle - crate::fb::screen_h() as i32 / 2).abs() < 160);
    }

    #[test]
    fn open_stream_stays_hidden_until_complete_centered_layout() {
        crate::fb::test_init_screen();
        let font = font();
        let first = "第一句先到。";
        let complete = "第一句先到。 第二句在關閉前到達。";
        let mut plan = plan_streaming_reply_async(&font, first);
        start_initial_reply_layout(&mut plan, true);
        assert!(plan.initial_buffering);
        assert!(plan.layout.is_none());
        assert!(plan.strokes.is_empty());
        assert!(!plan.first_damage_logged);
        let created_at = plan.created_at;

        assert!(recenter_undrawn_reply(&font, &mut plan, complete));
        wait_for_layout(&mut plan);
        assert_eq!(plan.created_at, created_at);
        assert_eq!(plan.strokes, plan_reply(&font, complete, None).strokes);
        let middle = (plan.region.y0 + plan.region.y1) / 2;
        assert!((middle - crate::fb::screen_h() as i32 / 2).abs() < 160);
    }

    #[test]
    fn stream_close_without_restart_still_uses_two_axis_centering() {
        crate::fb::test_init_screen();
        let font = font();
        let mut plan = plan_streaming_reply_async(&font, "第一句先到。等待後文。");
        start_initial_reply_layout(&mut plan, true);
        assert!(plan.layout.is_none());
        start_initial_reply_layout(&mut plan, false);
        wait_for_layout(&mut plan);
        let middle = (plan.region.y0 + plan.region.y1) / 2;
        assert!((middle - crate::fb::screen_h() as i32 / 2).abs() < 160);
    }

    #[test]
    fn zero_fitting_source_lines_still_produce_a_visible_ellipsis() {
        crate::fb::test_init_screen();
        let font = font();
        let bottom = crate::fb::screen_h() as i32 - MARGIN_Y;
        let plan = plan_reply(&font, &"無法放下的文字".repeat(20), Some(bottom));
        assert!(plan.truncated);
        assert_eq!(plan.visible_graphemes, 1);
        assert!(!plan.strokes.is_empty());
        for &(x, y) in plan.strokes.iter().flatten() {
            assert!(x >= MARGIN_X && x < crate::fb::screen_w() as i32 - MARGIN_X);
            assert!(y >= MARGIN_Y && y < crate::fb::screen_h() as i32);
        }
    }

    #[test]
    fn page_guard_overflow_marker_is_visible_and_idempotent() {
        crate::fb::test_init_screen();
        let font = font();
        let mut plan = plan_reply(&font, "已顯示的回答。", Some(MARGIN_Y));
        let strokes_before = plan.strokes.len();
        let graphemes_before = plan.visible_graphemes;
        append_overflow_marker(&font, &mut plan);
        assert!(plan.truncated);
        assert!(plan.strokes.len() > strokes_before);
        assert_eq!(plan.visible_graphemes, graphemes_before + 1);
        assert!(plan.region.y1 >= crate::fb::screen_h() as i32 - MARGIN_Y);
        let strokes_after = plan.strokes.len();
        append_overflow_marker(&font, &mut plan);
        assert_eq!(plan.strokes.len(), strokes_after);
    }
}
