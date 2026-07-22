//! MagicPaper's text layout and handwriting-stroke pipeline: rasterize text, thin it to
//! single-pixel pen paths (Zhang-Suen), trace them into ordered strokes, and
//! yield them for stroke-by-stroke animation.

use ab_glyph::{Font, Glyph, PxScale, ScaleFont};
use unicode_segmentation::UnicodeSegmentation;

use crate::fonts::{FontBook, FontId};

pub struct Line {
    pub width: usize,
    pub height: usize,
    /// Bit mask of inked pixels, row-major.
    pub mask: Vec<bool>,
}

/// Rasterize one line of text at `px` height into a boolean mask.
pub fn rasterize_line(fonts: &FontBook, text: &str, px: f32) -> Line {
    rasterize_line_with(fonts, fonts.selected(), text, px)
}

/// Rasterize fixed interface text with the dedicated UI face. Handwriting
/// selection and calibration must never change menus, lists or help pages.
pub fn rasterize_ui_line(fonts: &FontBook, text: &str, px: f32) -> Line {
    rasterize_line_with(fonts, FontId::Ui, text, px)
}

pub fn rasterize_line_with(fonts: &FontBook, primary: FontId, text: &str, px: f32) -> Line {
    let selected: Vec<(FontId, char)> = text
        .chars()
        .map(|c| {
            let (id, _) = fonts.resolve(primary, c);
            (id, c)
        })
        .collect();
    let baseline = selected
        .iter()
        .map(|(id, _)| {
            fonts
                .font(*id)
                .as_scaled(PxScale::from(fonts.calibrated_px(*id, px)))
                .ascent()
        })
        .fold(0.0f32, f32::max);
    let font_height = selected
        .iter()
        .map(|(id, _)| {
            fonts
                .font(*id)
                .as_scaled(PxScale::from(fonts.calibrated_px(*id, px)))
                .height()
        })
        .fold(px, f32::max);
    let mut glyphs: Vec<(FontId, Glyph)> = Vec::new();
    let mut caret = 0.0f32;
    let mut prev: Option<(FontId, ab_glyph::GlyphId)> = None;
    for (font_id, c) in selected {
        let font = fonts.font(font_id);
        let calibrated_px = fonts.calibrated_px(font_id, px);
        let scaled = font.as_scaled(PxScale::from(calibrated_px));
        let id = scaled.glyph_id(c);
        if let Some((_, p)) = prev.filter(|(previous_font, _)| *previous_font == font_id) {
            caret += scaled.kern(p, id);
        }
        let mut g = id.with_scale(PxScale::from(calibrated_px));
        g.position = ab_glyph::point(caret, baseline);
        caret += scaled.h_advance(id);
        glyphs.push((font_id, g));
        prev = Some((font_id, id));
    }
    let width = (caret.ceil() as usize + 4).max(1);
    let height = (font_height.ceil() as usize + 4).max(1);
    let mut mask = vec![false; width * height];
    for (font_id, g) in glyphs {
        let font = fonts.font(font_id);
        if let Some(outline) = font.outline_glyph(g) {
            let bounds = outline.px_bounds();
            outline.draw(|x, y, cov| {
                if cov > 0.5 {
                    let px_x = bounds.min.x as i32 + x as i32;
                    let px_y = bounds.min.y as i32 + y as i32;
                    if px_x >= 0 && px_y >= 0 && (px_x as usize) < width && (px_y as usize) < height
                    {
                        mask[px_y as usize * width + px_x as usize] = true;
                    }
                }
            });
        }
    }
    Line {
        width,
        height,
        mask,
    }
}

/// Measure the advance width of text at `px` without rasterizing.
pub fn measure(fonts: &FontBook, text: &str, px: f32) -> f32 {
    measure_with(fonts, fonts.selected(), text, px)
}

pub fn measure_ui(fonts: &FontBook, text: &str, px: f32) -> f32 {
    measure_with(fonts, FontId::Ui, text, px)
}

pub fn measure_with(fonts: &FontBook, primary: FontId, text: &str, px: f32) -> f32 {
    let mut caret = 0.0f32;
    let mut prev: Option<(FontId, ab_glyph::GlyphId)> = None;
    for c in text.chars() {
        let (font_id, font) = fonts.resolve(primary, c);
        let scaled = font.as_scaled(PxScale::from(fonts.calibrated_px(font_id, px)));
        let id = scaled.glyph_id(c);
        if let Some((_, p)) = prev.filter(|(previous_font, _)| *previous_font == font_id) {
            caret += scaled.kern(p, id);
        }
        caret += scaled.h_advance(id);
        prev = Some((font_id, id));
    }
    caret
}

/// Measure the tallest resolved face used by one line.  Handwriting faces
/// have independent calibration factors, so a fixed `base_px * 1.2` line
/// height can overlap lines or run off the bottom after font calibration.
pub fn line_height(fonts: &FontBook, text: &str, px: f32) -> f32 {
    text.chars()
        .map(|character| {
            let (font_id, font) = fonts.resolve(fonts.selected(), character);
            font.as_scaled(PxScale::from(fonts.calibrated_px(font_id, px)))
                .height()
        })
        .fold(px, f32::max)
}

/// Zhang-Suen thinning: reduce the mask to 1px-wide skeleton lines.
pub fn thin(line: &mut Line) {
    let (w, h) = (line.width, line.height);
    let idx = |x: usize, y: usize| y * w + x;
    loop {
        let mut changed = false;
        for phase in 0..2 {
            let mut to_clear = Vec::new();
            for y in 1..h.saturating_sub(1) {
                for x in 1..w.saturating_sub(1) {
                    if !line.mask[idx(x, y)] {
                        continue;
                    }
                    let p = [
                        line.mask[idx(x, y - 1)],     // p2 N
                        line.mask[idx(x + 1, y - 1)], // p3 NE
                        line.mask[idx(x + 1, y)],     // p4 E
                        line.mask[idx(x + 1, y + 1)], // p5 SE
                        line.mask[idx(x, y + 1)],     // p6 S
                        line.mask[idx(x - 1, y + 1)], // p7 SW
                        line.mask[idx(x - 1, y)],     // p8 W
                        line.mask[idx(x - 1, y - 1)], // p9 NW
                    ];
                    let b: u32 = p.iter().map(|&v| v as u32).sum();
                    if !(2..=6).contains(&b) {
                        continue;
                    }
                    let mut a = 0;
                    for i in 0..8 {
                        if !p[i] && p[(i + 1) % 8] {
                            a += 1;
                        }
                    }
                    if a != 1 {
                        continue;
                    }
                    let (c1, c2) = if phase == 0 {
                        (!(p[0] && p[2] && p[4]), !(p[2] && p[4] && p[6]))
                    } else {
                        (!(p[0] && p[2] && p[6]), !(p[0] && p[4] && p[6]))
                    };
                    if c1 && c2 {
                        to_clear.push(idx(x, y));
                    }
                }
            }
            if !to_clear.is_empty() {
                changed = true;
                for i in to_clear {
                    line.mask[i] = false;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

/// Trace the skeleton into polyline strokes, ordered left-to-right so the
/// animation writes like a hand.
pub fn trace(line: &Line) -> Vec<Vec<(i32, i32)>> {
    let (w, h) = (line.width, line.height);
    let at = |x: i32, y: i32| -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && line.mask[y as usize * w + x as usize]
    };
    let neighbors = |x: i32, y: i32| -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        for dy in -1..=1i32 {
            for dx in -1..=1i32 {
                if (dx != 0 || dy != 0) && at(x + dx, y + dy) {
                    out.push((x + dx, y + dy));
                }
            }
        }
        out
    };

    let mut visited = vec![false; w * h];
    let vis = |v: &mut Vec<bool>, x: i32, y: i32| {
        v[y as usize * w + x as usize] = true;
    };
    let seen = |v: &Vec<bool>, x: i32, y: i32| -> bool { v[y as usize * w + x as usize] };

    // Endpoints first (degree 1), then any remaining pixels (loops).
    let mut starts: Vec<(i32, i32)> = Vec::new();
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if at(x, y) && neighbors(x, y).len() == 1 {
                starts.push((x, y));
            }
        }
    }
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            if at(x, y) {
                starts.push((x, y));
            }
        }
    }

    let mut strokes: Vec<Vec<(i32, i32)>> = Vec::new();
    for (sx, sy) in starts {
        if seen(&visited, sx, sy) {
            continue;
        }
        let mut path = vec![(sx, sy)];
        vis(&mut visited, sx, sy);
        let (mut cx, mut cy) = (sx, sy);
        loop {
            let next = neighbors(cx, cy)
                .into_iter()
                .find(|&(nx, ny)| !seen(&visited, nx, ny));
            match next {
                Some((nx, ny)) => {
                    vis(&mut visited, nx, ny);
                    path.push((nx, ny));
                    cx = nx;
                    cy = ny;
                }
                None => break,
            }
        }
        if path.len() >= 3 {
            strokes.push(path);
        }
    }
    strokes.sort_by_key(|s| s.iter().map(|&(x, _)| x).min().unwrap_or(0));
    strokes
}

#[derive(Debug)]
struct WrapPiece {
    text: String,
    space_before: bool,
}

fn is_cjk_break_char(c: char) -> bool {
    matches!(
        c,
        '\u{2e80}'..='\u{2fff}'
            | '\u{3000}'..='\u{303f}'
            | '\u{31c0}'..='\u{31ef}'
            | '\u{3400}'..='\u{4dbf}'
            | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}'
            | '\u{fe30}'..='\u{fe4f}'
            | '\u{ff00}'..='\u{ffef}'
            | '\u{20000}'..='\u{2fa1f}'
    )
}

fn is_closing_punctuation(c: char) -> bool {
    matches!(
        c,
        '，' | '。'
            | '、'
            | '！'
            | '？'
            | '；'
            | '：'
            | '…'
            | '—'
            | '）'
            | '】'
            | '》'
            | '〉'
            | '」'
            | '』'
            | '〕'
            | '］'
            | '｝'
    )
}

fn wrap_pieces(text: &str) -> Vec<WrapPiece> {
    let mut pieces: Vec<WrapPiece> = Vec::new();
    let mut word = String::new();
    let mut word_space_before = false;
    let mut pending_space = false;

    let flush_word = |pieces: &mut Vec<WrapPiece>, word: &mut String, space_before: bool| {
        if !word.is_empty() {
            pieces.push(WrapPiece {
                text: std::mem::take(word),
                space_before,
            });
        }
    };

    for c in text.chars() {
        if c.is_whitespace() {
            flush_word(&mut pieces, &mut word, word_space_before);
            pending_space = true;
            continue;
        }

        if is_cjk_break_char(c) {
            flush_word(&mut pieces, &mut word, word_space_before);
            if is_closing_punctuation(c) && !pending_space {
                if let Some(previous) = pieces.last_mut() {
                    previous.text.push(c);
                    continue;
                }
            }
            pieces.push(WrapPiece {
                text: c.to_string(),
                space_before: pending_space,
            });
            pending_space = false;
            continue;
        }

        if word.is_empty() {
            word_space_before = pending_space;
            pending_space = false;
        }
        word.push(c);
    }
    flush_word(&mut pieces, &mut word, word_space_before);
    pieces
}

/// Word-wrap `text` to lines that fit `max_px` at scale `px`.
///
/// Latin text keeps word boundaries while CJK text can break between glyphs;
/// Chinese replies normally contain no spaces, so `split_whitespace` alone
/// would otherwise render an entire paragraph beyond the screen edge.
pub fn wrap(font: &FontBook, text: &str, px: f32, max_px: f32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.lines() {
        let mut cur = String::new();
        for piece in wrap_pieces(para) {
            let separator = if piece.space_before && !cur.is_empty() {
                " "
            } else {
                ""
            };
            let cand = format!("{cur}{separator}{}", piece.text);
            if measure(font, &cand, px) <= max_px {
                cur = cand;
            } else {
                if !cur.is_empty() {
                    lines.push(std::mem::take(&mut cur));
                }

                // A URL, identifier or other unbroken Latin token may be
                // wider than the entire Move panel.  Split it by grapheme
                // cluster so its centered origin can never become negative;
                // ordinary words and CJK pieces keep their natural wrapping.
                let chunks = hard_wrap_piece(font, &piece.text, px, max_px);
                let chunk_count = chunks.len();
                for (index, chunk) in chunks.into_iter().enumerate() {
                    if index + 1 == chunk_count {
                        cur = chunk;
                    } else {
                        lines.push(chunk);
                    }
                }
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }
    }
    lines
}

fn hard_wrap_piece(font: &FontBook, text: &str, px: f32, max_px: f32) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for grapheme in UnicodeSegmentation::graphemes(text, true) {
        let candidate = format!("{current}{grapheme}");
        if current.is_empty() || measure(font, &candidate, px) <= max_px {
            current = candidate;
        } else {
            chunks.push(std::mem::take(&mut current));
            current.push_str(grapheme);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

#[cfg(test)]
#[path = "script/tests.rs"]
mod tests;
