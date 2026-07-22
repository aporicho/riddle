//! Reply block measurement, centering, truncation and overflow marks.

use std::time::Instant;

use unicode_segmentation::UnicodeSegmentation;

use crate::fb::{screen_h, screen_w, BBox};
use crate::{fonts, script};

use super::WritePlan;

const REPLY_PX: f32 = 96.0;
const MIN_REPLY_PX: f32 = 42.0;
const REPLY_PX_STEP: f32 = 6.0;
pub(super) const MARGIN_X: i32 = 72;
pub(super) const MARGIN_Y: i32 = 84;
const RASTER_SAFETY: f32 = 8.0;
const WOBBLE_SAFETY: i32 = 6;

#[derive(Debug)]
struct ReplyLayout {
    lines: Vec<String>,
    px: f32,
    heights: Vec<i32>,
    gap: i32,
    total_h: i32,
    truncated: bool,
    /// No source line fits at the requested continuation position. Render a
    /// standalone ellipsis at the safe bottom edge instead of dropping the
    /// entire tail without a visible signal.
    overflow_only: bool,
}

/// Lay out reply text and produce screen-space strokes. `y_start` continues a
/// streamed reply below its previous chunk; None places the first chunk.
pub(in crate::app) fn plan_reply(
    font: &fonts::FontBook,
    text: &str,
    y_start: Option<i32>,
) -> WritePlan {
    let screen_height = screen_h() as i32;
    let top = MARGIN_Y;
    let bottom = screen_height - MARGIN_Y;
    let start = y_start.map_or(top, |value| value.max(top));
    let available_h = (bottom - start).max(0);
    let layout = fit_reply(font, text, (available_h - WOBBLE_SAFETY).max(0));
    let mut y = match y_start {
        Some(_) => start,
        None => top + (available_h - layout.total_h).max(0) / 2,
    };
    if layout.overflow_only {
        y = (bottom - layout.total_h - WOBBLE_SAFETY).max(top);
    }
    let mut strokes = Vec::new();
    let mut region = BBox::empty();
    let mut seed = 0x1234u32;
    let mut jitter = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        ((seed >> 16) % 7) as i32 - 3
    };

    for (index, line_text) in layout.lines.iter().enumerate() {
        let mut raster = script::rasterize_line(font, line_text, layout.px);
        script::thin(&mut raster);
        let line_strokes = script::trace(&raster);
        let x0 = if layout.overflow_only {
            (screen_w() as i32 - MARGIN_X - raster.width as i32).max(MARGIN_X)
        } else {
            ((screen_w() as i32 - raster.width as i32) / 2).max(MARGIN_X)
        };
        let wobble = jitter();
        for stroke in line_strokes {
            let mapped: Vec<(i32, i32)> = stroke
                .iter()
                .map(|&(sx, sy)| (x0 + sx, y + sy + wobble))
                .collect();
            for &(x, line_y) in &mapped {
                region.add(x, line_y, 5);
            }
            strokes.push(mapped);
        }
        y += layout.heights[index];
        if index + 1 < layout.lines.len() {
            y += layout.gap;
        }
    }
    if layout.overflow_only && strokes.is_empty() {
        for mapped in manual_overflow_marker(false) {
            for &(x, line_y) in &mapped {
                region.add(x, line_y, 5);
            }
            strokes.push(mapped);
        }
    }

    let visible_graphemes = visible_grapheme_count(&layout.lines.join("\n"));
    let next_y = if layout.lines.is_empty() {
        y
    } else {
        (y + layout.gap).min(bottom)
    };

    WritePlan {
        created_at: Instant::now(),
        first_damage_logged: false,
        strokes,
        stroke_i: 0,
        point_i: 0,
        region,
        next_y,
        layout: None,
        queued_text: String::new(),
        layout_font: None,
        initial_buffering: false,
        visible_graphemes,
        truncated: layout.truncated,
    }
}

fn fit_reply(font: &fonts::FontBook, text: &str, available_h: i32) -> ReplyLayout {
    let max_w = (screen_w() as i32 - 2 * MARGIN_X) as f32 - RASTER_SAFETY;
    let mut px = REPLY_PX;
    loop {
        let lines = script::wrap(font, text, px, max_w);
        let heights = reply_line_heights(font, &lines, px);
        let gap = reply_line_gap(px);
        let total_h = reply_block_height(&heights, gap);
        if total_h <= available_h || px <= MIN_REPLY_PX {
            return if total_h <= available_h {
                ReplyLayout {
                    lines,
                    px,
                    heights,
                    gap,
                    total_h,
                    truncated: false,
                    overflow_only: false,
                }
            } else {
                truncate_reply(font, lines, px, max_w, available_h)
            };
        }
        px = (px - REPLY_PX_STEP).max(MIN_REPLY_PX);
    }
}

fn reply_line_heights(font: &fonts::FontBook, lines: &[String], px: f32) -> Vec<i32> {
    lines
        .iter()
        .map(|line| (script::line_height(font, line, px) + 4.0).ceil() as i32)
        .collect()
}

fn reply_line_gap(px: f32) -> i32 {
    (px * 0.18).ceil().max(8.0) as i32
}

fn reply_block_height(heights: &[i32], gap: i32) -> i32 {
    heights.iter().sum::<i32>() + gap * heights.len().saturating_sub(1) as i32
}

fn truncate_reply(
    font: &fonts::FontBook,
    mut lines: Vec<String>,
    px: f32,
    max_w: f32,
    available_h: i32,
) -> ReplyLayout {
    let gap = reply_line_gap(px);
    let heights = reply_line_heights(font, &lines, px);
    let mut used = 0;
    let mut keep = 0;
    for height in &heights {
        let candidate = used + if keep == 0 { 0 } else { gap } + height;
        if candidate > available_h {
            break;
        }
        used = candidate;
        keep += 1;
    }
    lines.truncate(keep);
    let overflow_only = lines.is_empty();
    if overflow_only {
        lines.push(overflow_marker_text(font, px, max_w));
    } else if let Some(last) = lines.last_mut() {
        append_ellipsis(font, last, px, max_w);
    }
    let heights = reply_line_heights(font, &lines, px);
    let total_h = reply_block_height(&heights, gap);
    ReplyLayout {
        lines,
        px,
        heights,
        gap,
        total_h,
        truncated: true,
        overflow_only,
    }
}

fn overflow_marker_text(font: &fonts::FontBook, px: f32, max_w: f32) -> String {
    if script::measure(font, "…", px) <= max_w {
        "…".to_owned()
    } else {
        ".".to_owned()
    }
}

fn append_ellipsis(font: &fonts::FontBook, line: &mut String, px: f32, max_w: f32) {
    while !line.is_empty() && script::measure(font, &format!("{line}…"), px) > max_w {
        let Some((last, _)) =
            UnicodeSegmentation::grapheme_indices(line.as_str(), true).next_back()
        else {
            break;
        };
        line.truncate(last);
        while line.chars().last().is_some_and(char::is_whitespace) {
            line.pop();
        }
    }
    line.push('…');
}

/// Add one unmistakable footer mark when the streaming page guard rejects a
/// new chunk before a normal truncating layout can run.
pub(in crate::app) fn append_overflow_marker(font: &fonts::FontBook, plan: &mut WritePlan) {
    if plan.truncated {
        return;
    }
    let max_w = (screen_w() as i32 - 2 * MARGIN_X) as f32 - RASTER_SAFETY;
    let marker = overflow_marker_text(font, MIN_REPLY_PX, max_w);
    let mut raster = script::rasterize_line(font, &marker, MIN_REPLY_PX);
    script::thin(&mut raster);
    let marker_strokes = script::trace(&raster);
    let x0 = (screen_w() as i32 - MARGIN_X - raster.width as i32).max(MARGIN_X);
    let y0 = (screen_h() as i32 - MARGIN_Y + (MARGIN_Y - raster.height as i32) / 2).clamp(
        MARGIN_Y,
        screen_h() as i32 - raster.height as i32 - WOBBLE_SAFETY,
    );
    let mapped_strokes = if marker_strokes.is_empty() {
        manual_overflow_marker(true)
    } else {
        marker_strokes
            .into_iter()
            .map(|stroke| stroke.into_iter().map(|(x, y)| (x0 + x, y0 + y)).collect())
            .collect()
    };
    for mapped in mapped_strokes {
        for &(x, y) in &mapped {
            plan.region.add(x, y, 5);
        }
        plan.strokes.push(mapped);
    }
    plan.visible_graphemes = plan.visible_graphemes.saturating_add(1);
    plan.truncated = true;
}

fn manual_overflow_marker(in_footer: bool) -> Vec<Vec<(i32, i32)>> {
    let y = if in_footer {
        screen_h() as i32 - MARGIN_Y / 2
    } else {
        screen_h() as i32 - MARGIN_Y - 12
    };
    let right = screen_w() as i32 - MARGIN_X - 4;
    [right - 20, right - 10, right]
        .into_iter()
        .map(|x| vec![(x, y)])
        .collect()
}

/// Count only clusters that leave visible ink. Combining sequences and emoji
/// stay one unit; layout whitespace does not artificially extend dwell time.
pub(super) fn visible_grapheme_count(text: &str) -> usize {
    UnicodeSegmentation::graphemes(text, true)
        .filter(|cluster| !cluster.chars().all(char::is_whitespace))
        .count()
}
