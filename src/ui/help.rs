//! MagicPaper's instruction manual: a lone, large "?" or a local help command summons it.
//! the diary's gestures; touching the pen to the page dismisses it. Detection
//! is local geometry — no oracle — so the guide works even with no network.

use crate::fb::{screen_h, screen_w, BBox};
use crate::fonts::FontBook;
use crate::script;
use crate::surface::{Surface, BLACK, WHITE};

use super::pointer::{invert_mono, Gesture, HitRect, Point, PointerTool};

/// Does the committed ink look like a single big "?" (with or without its
/// dot)? Deliberately forgiving: a false positive only shows the guide.
pub fn looks_like_question_mark(strokes: &[Vec<(i32, i32, i32)>]) -> bool {
    if strokes.is_empty() || strokes.len() > 3 {
        return false;
    }
    let main_i = (0..strokes.len())
        .max_by_key(|&i| strokes[i].len())
        .unwrap();
    let main = &strokes[main_i];
    if main.len() < 12 {
        return false;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for &(x, y, _) in main {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let (w, h) = (x1 - x0, y1 - y0);
    // Big, and taller than wide: a lone glyph, not a line of writing.
    if h < 280 || w < 70 || h < w {
        return false;
    }
    // Any other stroke must be the dot: small, low, roughly under the glyph.
    for (i, s) in strokes.iter().enumerate() {
        if i == main_i {
            continue;
        }
        let (mut dx0, mut dy0, mut dx1, mut dy1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(x, y, _) in s {
            dx0 = dx0.min(x);
            dy0 = dy0.min(y);
            dx1 = dx1.max(x);
            dy1 = dy1.max(y);
        }
        if (dx1 - dx0).max(dy1 - dy0) > 90 {
            return false;
        }
        if (dy0 + dy1) / 2 < y0 + h * 60 / 100 {
            return false;
        }
        if (dx0 + dx1) / 2 < x0 - 80 || (dx0 + dx1) / 2 > x1 + 80 {
            return false;
        }
    }
    // Normalize to top-down drawing order.
    let mut pts: Vec<(i32, i32)> = main.iter().map(|&(x, y, _)| (x, y)).collect();
    if pts[0].1 > pts[pts.len() - 1].1 {
        pts.reverse();
    }
    let start = pts[0];
    let end = pts[pts.len() - 1];
    if start.1 > y0 + h * 40 / 100 || end.1 < y0 + h * 55 / 100 {
        return false;
    }
    // The top arcs across most of the width…
    let (mut top_minx, mut top_maxx, mut top_maxx_y) = (i32::MAX, i32::MIN, 0);
    for &(x, y) in &pts {
        if y <= y0 + h * 45 / 100 {
            if x > top_maxx {
                top_maxx = x;
                top_maxx_y = y;
            }
            top_minx = top_minx.min(x);
        }
    }
    if top_maxx == i32::MIN || top_maxx - top_minx < w * 55 / 100 {
        return false;
    }
    // …and comes back DOWN on the right (rules out the flat bar of a "7").
    if top_maxx_y < y0 + h * 8 / 100 {
        return false;
    }
    // The descender stays narrow.
    let (mut bot_minx, mut bot_maxx) = (i32::MAX, i32::MIN);
    for &(x, y) in &pts {
        if y >= y0 + h * 66 / 100 {
            bot_minx = bot_minx.min(x);
            bot_maxx = bot_maxx.max(x);
        }
    }
    if bot_maxx != i32::MIN && bot_maxx - bot_minx > w * 60 / 100 {
        return false;
    }
    true
}

const TITLE: &str = "MagicPaper 使用说明";
/// Takeover mode: riddle owns touch and the power button.
const BODY_TAKEOVER: &[&str] = &[
    "书写后停笔，MP 会读取墨迹并回答。",
    "等待时仍可继续写，新笔迹永远优先。",
    "",
    "写「任务」：打开定时任务列表。",
    "写「TODO」：打开待办事项列表。",
    "写「历史」：查看最近九段对话。",
    "写「字体」：切换字体并校准大小。",
    "写「设置」：调整刷新、停留和字体。",
    "写「帮助」或 help：打开本说明。",
    "写 read：打开 KOReader 书库。",
    "写 read 书名：直接打开指定书籍。",
    "写「刷新」或 refresh：全屏清除残影。",
    "",
    "在列表中横划一项即可删除。",
    "任务右侧：勾为启用，叉为停用。",
    "点击空白处退出列表。",
    "",
    "画一个大问号也能打开本说明。",
    "翻转笔端可以擦除。",
    "快速按三次电源键进入或退出 MP。",
    "单按电源键可休眠或唤醒。",
    "五指触碰仍可紧急退出。",
];
/// Hosted mode: Remagic owns the device lifecycle and application switching.
const BODY_HOSTED: &[&str] = &[
    "书写后停笔，MP 会读取墨迹并回答。",
    "等待时仍可继续写，新笔迹永远优先。",
    "",
    "写「任务」：打开定时任务列表。",
    "写「TODO」：打开待办事项列表。",
    "写「历史」：查看最近九段对话。",
    "写「字体」：切换字体并校准大小。",
    "写「设置」：调整刷新、停留和字体。",
    "写「帮助」或 help：打开本说明。",
    "写 read：打开 KOReader 书库。",
    "写 read 书名：直接打开指定书籍。",
    "写「刷新」或 refresh：全屏清除残影。",
    "",
    "在列表中横划一项即可删除。",
    "任务右侧：勾为启用，叉为停用。",
    "点击空白处退出列表。",
    "",
    "画一个大问号也能打开本说明。",
    "翻转笔端可以擦除。",
    "单按电源键返回应用管理器。",
    "快速按三次电源键返回原版界面。",
];
const FOOTER: &str = "关闭说明";

const TITLE_PX: f32 = 72.0;
const BODY_PX: f32 = 42.0;
const FOOTER_PX: f32 = 36.0;
const PAD: usize = 64;

fn fitted_base_sizes(body_lines: usize, page_h: usize) -> (f32, f32, f32) {
    let raw_text_height =
        TITLE_PX * 1.4 + BODY_PX * 1.3 * (body_lines as f32 + 0.5) + FOOTER_PX * 1.4;
    let available_text_height = page_h.saturating_sub(2 * PAD + 40) as f32;
    let fit = (available_text_height / raw_text_height).min(1.0);
    (TITLE_PX * fit, BODY_PX * fit, FOOTER_PX * fit)
}

/// The open guide panel: remembers the pixels it covered.
pub struct Help {
    pub region: BBox,
    panel_rect: HitRect,
    close_rect: HitRect,
    saved: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpHit {
    Close,
    Panel,
    Outside,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HelpAction {
    Close,
    Outside,
    Consume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HelpPreview {
    tool: PointerTool,
    rect: HitRect,
    start: Point,
    pressed: bool,
}

impl HelpPreview {
    pub const fn rect(self) -> HitRect {
        self.rect
    }

    pub const fn is_visible(self) -> bool {
        self.pressed
    }

    pub fn update(&mut self, point: Point) -> bool {
        let next = self.rect.contains(point);
        let changed = self.pressed != next;
        self.pressed = next;
        changed
    }

    pub fn render(self, surface: &mut Surface) {
        if self.pressed {
            invert_mono(surface, self.rect);
        }
    }

    pub fn release_gesture(self, end: Point) -> Gesture {
        if self.pressed {
            Gesture::Tap {
                tool: self.tool,
                at: end,
            }
        } else {
            Gesture::Swipe {
                tool: self.tool,
                from: self.start,
                to: self.start,
                bounds: HitRect::from_xywh(self.start.x, self.start.y, 1, 1),
            }
        }
    }
}

/// Draw the guide panel centered on the page; returns it for later dismissal.
/// The gesture list depends on the display mode: takeover owns raw device
/// controls, while hosted mode delegates switching and power to Remagic.
pub fn show(surf: &mut Surface, font: &FontBook, takeover: bool) -> Help {
    let body = if takeover { BODY_TAKEOVER } else { BODY_HOSTED };
    let (title_base_px, body_base_px, footer_base_px) = fitted_base_sizes(body.len(), screen_h());
    let title_px = title_base_px;
    let body_px = body_base_px;
    let footer_px = footer_base_px;
    let title_h = (title_px * 1.4) as usize;
    let line_h = (body_px * 1.3) as usize;
    let footer_h = (footer_px * 1.4) as usize;

    let mut wmax = script::measure_ui(font, TITLE, title_base_px).max(script::measure_ui(
        font,
        FOOTER,
        footer_base_px,
    ));
    for l in body {
        wmax = wmax.max(script::measure_ui(font, l, body_base_px));
    }
    let pw = (wmax as usize + 2 * PAD).min(screen_w().saturating_sub(40));
    let ph = PAD + title_h + line_h / 2 + body.len() * line_h + footer_h + PAD;
    let px = (screen_w() - pw) / 2;
    let py = (screen_h().saturating_sub(ph)) / 2;

    let saved = surf.copy_rect(px, py, pw, ph);
    surf.fill_rect(px, py, pw, ph, WHITE);
    frame(surf, px, py, pw, ph, 4);
    frame(surf, px + 14, py + 14, pw - 28, ph - 28, 1);

    let mut y = py + PAD;
    blit_centered(surf, font, TITLE, title_base_px, px, pw, y);
    y += title_h + line_h / 2;
    for l in body {
        if !l.is_empty() {
            blit_centered(surf, font, l, body_base_px, px, pw, y);
        }
        y += line_h;
    }
    let footer_width = script::measure_ui(font, FOOTER, footer_base_px) as usize;
    let close_w = (footer_width + 72).min(pw.saturating_sub(PAD * 2)).max(160);
    let close_h = footer_h + 20;
    let close_x = px + pw.saturating_sub(close_w) / 2;
    let close_y = y.saturating_sub(10);
    let close_rect = HitRect::from_xywh(
        close_x as i32,
        close_y as i32,
        close_w as i32,
        close_h as i32,
    );
    draw_hit_frame(surf, close_rect, 2);
    blit_centered(surf, font, FOOTER, footer_base_px, close_x, close_w, y);

    let mut region = BBox::empty();
    region.add(px as i32, py as i32, 2);
    region.add((px + pw) as i32, (py + ph) as i32, 2);
    Help {
        region,
        panel_rect: HitRect::from_xywh(px as i32, py as i32, pw as i32, ph as i32),
        close_rect,
        saved,
    }
}

impl Help {
    #[cfg(test)]
    pub const fn panel_rect(&self) -> HitRect {
        self.panel_rect
    }

    #[cfg(test)]
    pub const fn close_rect(&self) -> HitRect {
        self.close_rect
    }

    pub fn hit_test(&self, point: Point) -> HelpHit {
        if self.close_rect.contains(point) {
            HelpHit::Close
        } else if self.panel_rect.contains(point) {
            HelpHit::Panel
        } else {
            HelpHit::Outside
        }
    }

    pub fn begin_preview(&self, tool: PointerTool, point: Point) -> Option<HelpPreview> {
        (self.hit_test(point) == HelpHit::Close).then_some(HelpPreview {
            tool,
            rect: self.close_rect,
            start: point,
            pressed: true,
        })
    }

    /// Classify a modal contact without drawing it. Runtime decides whether
    /// `Outside` also dismisses. Ordinary panel taps and every non-tap gesture
    /// are consumed so modal input cannot fall through to page ink.
    pub fn interact(&self, gesture: Gesture) -> Option<HelpAction> {
        let Gesture::Tap { at, .. } = gesture else {
            return Some(HelpAction::Consume);
        };
        Some(match self.hit_test(at) {
            HelpHit::Close => HelpAction::Close,
            HelpHit::Panel => HelpAction::Consume,
            HelpHit::Outside => HelpAction::Outside,
        })
    }

    /// Put back what the panel covered; returns the region to refresh.
    pub fn dismiss(self, surf: &mut Surface) -> BBox {
        let panel = self.panel_rect;
        surf.paste_rect(
            panel.x0 as usize,
            panel.y0 as usize,
            panel.width() as usize,
            panel.height() as usize,
            &self.saved,
        );
        self.region
    }
}

/// Replace the page with the full-screen sleep card; returns the saved page
/// pixels so waking can restore them exactly.
pub fn show_sleep(surf: &mut Surface, font: &FontBook) -> Vec<u8> {
    let (w, h) = (screen_w(), screen_h());
    let saved = surf.copy_rect(0, 0, w, h);
    surf.fill_rect(0, 0, w, h, WHITE);
    frame(surf, 48, 48, w - 96, h - 96, 4);
    frame(surf, 66, 66, w - 132, h - 132, 1);
    let y = h * 38 / 100;
    blit_centered(surf, font, "MagicPaper sleeps.", 116.0, 0, w, y);
    blit_centered(
        surf,
        font,
        "Press the button to wake it.",
        56.0,
        0,
        w,
        y + 230,
    );
    saved
}

pub fn restore_sleep(surf: &mut Surface, saved: &[u8]) {
    surf.paste_rect(0, 0, screen_w(), screen_h(), saved);
}

fn frame(surf: &mut Surface, x: usize, y: usize, w: usize, h: usize, t: usize) {
    surf.fill_rect(x, y, w, t, BLACK);
    surf.fill_rect(x, y + h - t, w, t, BLACK);
    surf.fill_rect(x, y, t, h, BLACK);
    surf.fill_rect(x + w - t, y, t, h, BLACK);
}

fn draw_hit_frame(surf: &mut Surface, rect: HitRect, thickness: usize) {
    frame(
        surf,
        rect.x0 as usize,
        rect.y0 as usize,
        rect.width() as usize,
        rect.height() as usize,
        thickness,
    );
}

fn blit_centered(
    surf: &mut Surface,
    font: &FontBook,
    text: &str,
    px_size: f32,
    panel_x: usize,
    panel_w: usize,
    y: usize,
) {
    let line = script::rasterize_ui_line(font, text, px_size);
    let x = panel_x + panel_w.saturating_sub(line.width) / 2;
    for row in 0..line.height {
        for col in 0..line.width {
            if line.mask[row * line.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
}

#[cfg(test)]
#[path = "help/tests.rs"]
mod tests;
