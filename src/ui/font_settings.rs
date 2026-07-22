//! Paper-native font selection and per-font visual-size calibration.

use crate::fb::{screen_h, screen_w};
use crate::fonts::{FontBook, FontId, MAX_SCALE_PERCENT, MIN_SCALE_PERCENT};
use crate::script;
use crate::surface::{Surface, BLACK, FADED, WHITE};

use super::pointer::{invert_mono, Gesture, HitRect, Point, PointerTool};

const SIDE: usize = 100;
const LIST_TOP: usize = 260;
const ROW_H: usize = 330;
const SLIDER_SIDE: usize = 180;
const SLIDER_Y_OFFSET: usize = 244;
const SLIDER_CONTROL_PAD_X: i32 = 30;
const SLIDER_CONTROL_HALF_H: i32 = 38;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Select(FontId),
    SetScale(FontId, u16),
    Dismiss,
    Redraw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Card(FontId),
    Slider(FontId),
    Blank,
}

/// Transient component-owned feedback for a font card or its slider. Slider
/// previews redraw the exact row from an unmodified save-under buffer; the
/// preference is persisted only after pointer release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    Card {
        tool: PointerTool,
        id: FontId,
        card: HitRect,
        slider_control: HitRect,
        start: Point,
        pressed: bool,
    },
    Slider {
        tool: PointerTool,
        id: FontId,
        geometry: RowGeometry,
        y0: usize,
        start: Point,
        percent: u16,
    },
}

impl Preview {
    pub const fn rect(self) -> HitRect {
        match self {
            Self::Card { card, .. } => card,
            Self::Slider { geometry, .. } => geometry.card,
        }
    }

    pub const fn is_visible(self) -> bool {
        matches!(self, Self::Card { pressed: true, .. } | Self::Slider { .. })
    }

    pub fn update(&mut self, point: Point) -> bool {
        match self {
            Self::Card {
                card,
                slider_control,
                pressed,
                ..
            } => {
                let next = card.contains(point) && !slider_control.contains(point);
                let changed = *pressed != next;
                *pressed = next;
                changed
            }
            Self::Slider {
                geometry, percent, ..
            } => {
                let next = slider_percent_for_geometry(*geometry, point.x);
                let changed = *percent != next;
                *percent = next;
                changed
            }
        }
    }

    pub fn render(self, surface: &mut Surface, fonts: &FontBook) {
        match self {
            Self::Card {
                card,
                pressed: true,
                ..
            } => invert_mono(surface, card),
            Self::Slider {
                id, y0, percent, ..
            } => {
                let _ = draw_row(surface, fonts, id, y0, percent);
            }
            _ => {}
        }
    }

    pub fn release_gesture(self, end: Point) -> Gesture {
        match self {
            Self::Card {
                tool,
                start: _,
                pressed: true,
                ..
            } => Gesture::Tap { tool, at: end },
            Self::Slider { tool, start, .. } => Gesture::Swipe {
                tool,
                from: start,
                to: end,
                bounds: HitRect::from_points(&[start, end])
                    .unwrap_or_else(|| HitRect::from_xywh(start.x, start.y, 1, 1)),
            },
            Self::Card { tool, start, .. } => no_op_gesture(tool, start),
        }
    }
}

fn no_op_gesture(tool: PointerTool, at: Point) -> Gesture {
    Gesture::Swipe {
        tool,
        from: at,
        to: at,
        bounds: HitRect::from_xywh(at.x, at.y, 1, 1),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RowGeometry {
    pub card: HitRect,
    pub title: HitRect,
    pub preview: HitRect,
    pub slider_control: HitRect,
    pub slider_x0: i32,
    pub slider_x1: i32,
    pub slider_y: i32,
}

#[derive(Clone, Copy, Debug)]
struct Row {
    id: FontId,
    y0: usize,
    geometry: RowGeometry,
}

pub struct FontPanel {
    saved: Vec<u8>,
    rows: Vec<Row>,
}

impl FontPanel {
    pub fn show(surf: &mut Surface, fonts: &FontBook) -> Self {
        let mut panel = Self {
            saved: surf.copy_rect(0, 0, screen_w(), screen_h()),
            rows: Vec::new(),
        };
        panel.redraw(surf, fonts);
        panel
    }

    pub fn redraw(&mut self, surf: &mut Surface, fonts: &FontBook) {
        surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
        self.rows.clear();

        blit_ui_centered(surf, fonts, "字体与大小", 82.0, 82);
        blit_ui_centered(
            surf,
            fonts,
            "点击字体切换 · 拖动横线校准大小 · 点击空白退出",
            40.0,
            screen_h().saturating_sub(105),
        );

        for (index, id) in fonts.available().enumerate() {
            let y0 = LIST_TOP + index * ROW_H;
            let percent = fonts.scale_percent(id);
            let geometry = draw_row(surf, fonts, id, y0, percent);
            self.rows.push(Row { id, y0, geometry });
        }
    }

    #[cfg(test)]
    pub fn row_geometry(&self, id: FontId) -> Option<RowGeometry> {
        self.rows
            .iter()
            .find(|row| row.id == id)
            .map(|row| row.geometry)
    }

    pub fn hit_test(&self, point: Point) -> Hit {
        if let Some(row) = self
            .rows
            .iter()
            .find(|row| row.geometry.slider_control.contains(point))
        {
            return Hit::Slider(row.id);
        }
        self.rows
            .iter()
            .find(|row| row.geometry.card.contains(point))
            .map_or(Hit::Blank, |row| Hit::Card(row.id))
    }

    pub fn begin_preview(&self, tool: PointerTool, point: Point) -> Option<Preview> {
        match self.hit_test(point) {
            Hit::Slider(id) => {
                let row = self.rows.iter().find(|row| row.id == id)?;
                Some(Preview::Slider {
                    tool,
                    id,
                    geometry: row.geometry,
                    y0: row.y0,
                    start: point,
                    percent: slider_percent(row, point.x),
                })
            }
            Hit::Card(id) => {
                let row = self.rows.iter().find(|row| row.id == id)?;
                Some(Preview::Card {
                    tool,
                    id,
                    card: row.geometry.card,
                    slider_control: row.geometry.slider_control,
                    start: point,
                    pressed: true,
                })
            }
            Hit::Blank => None,
        }
    }

    pub fn interact(&mut self, gesture: Gesture) -> Option<Action> {
        match gesture {
            Gesture::Tap { at, .. } => Some(match self.hit_test(at) {
                Hit::Slider(id) => {
                    let row = self.rows.iter().find(|row| row.id == id)?;
                    Action::SetScale(id, slider_percent(row, at.x))
                }
                Hit::Card(id) => Action::Select(id),
                Hit::Blank => Action::Dismiss,
            }),
            Gesture::Swipe { from, to, .. } | Gesture::Strike { from, to, .. } => {
                let row = self
                    .rows
                    .iter()
                    .find(|row| row.geometry.slider_control.contains(from));
                Some(row.map_or(Action::Redraw, |row| {
                    Action::SetScale(row.id, slider_percent(row, to.x))
                }))
            }
        }
    }

    pub fn dismiss(self, surf: &mut Surface) {
        surf.paste_rect(0, 0, screen_w(), screen_h(), &self.saved);
    }
}

fn slider_percent(row: &Row, x: i32) -> u16 {
    slider_percent_for_geometry(row.geometry, x)
}

fn slider_percent_for_geometry(geometry: RowGeometry, x: i32) -> u16 {
    let x = x.clamp(geometry.slider_x0, geometry.slider_x1);
    let span = (geometry.slider_x1 - geometry.slider_x0).max(1) as i64;
    let range = (MAX_SCALE_PERCENT - MIN_SCALE_PERCENT) as i64;
    (MIN_SCALE_PERCENT as i64 + (x - geometry.slider_x0) as i64 * range / span) as u16
}

fn draw_row(
    surf: &mut Surface,
    fonts: &FontBook,
    id: FontId,
    y0: usize,
    percent: u16,
) -> RowGeometry {
    let card = HitRect::from_xywh(
        SIDE as i32,
        (y0 + 4) as i32,
        screen_w().saturating_sub(SIDE * 2) as i32,
        ROW_H.saturating_sub(12) as i32,
    );
    surf.fill_rect(
        card.x0 as usize,
        card.y0 as usize,
        card.width() as usize,
        card.height() as usize,
        WHITE,
    );
    draw_frame(surf, card, 2, FADED);
    let marker = if id == fonts.selected() {
        "当前 · "
    } else {
        ""
    };
    let title = format!("{marker}{}  {percent}%", id.display_name());
    let title_rect = blit_ui_left(surf, fonts, &title, 58.0, SIDE + 20, y0 + 24);
    let stored_percent = fonts.scale_percent(id).max(1);
    let preview_base_px = 44.0 * percent as f32 / stored_percent as f32;
    let preview_rect = blit_handwriting_left(
        surf,
        fonts,
        id,
        "任务提醒 · 今日阅读 · MagicPaper",
        preview_base_px,
        SIDE + 20,
        y0 + 112,
    );
    let slider_x0 = SLIDER_SIDE as i32;
    let slider_x1 = screen_w().saturating_sub(SLIDER_SIDE) as i32;
    let slider_y = (y0 + SLIDER_Y_OFFSET) as i32;
    let slider_control = HitRect::from_xywh(
        slider_x0 - SLIDER_CONTROL_PAD_X,
        slider_y - SLIDER_CONTROL_HALF_H,
        slider_x1 - slider_x0 + SLIDER_CONTROL_PAD_X * 2,
        SLIDER_CONTROL_HALF_H * 2,
    );
    draw_frame(surf, slider_control, 2, FADED);
    draw_slider(surf, slider_x0, slider_x1, slider_y, percent);
    RowGeometry {
        card,
        title: title_rect,
        preview: preview_rect,
        slider_control,
        slider_x0,
        slider_x1,
        slider_y,
    }
}

fn slider_x(x0: i32, x1: i32, percent: u16) -> i32 {
    let range = (MAX_SCALE_PERCENT - MIN_SCALE_PERCENT).max(1) as i64;
    let value = percent.clamp(MIN_SCALE_PERCENT, MAX_SCALE_PERCENT) - MIN_SCALE_PERCENT;
    x0 + ((x1 - x0) as i64 * value as i64 / range) as i32
}

fn draw_slider(surf: &mut Surface, x0: i32, x1: i32, y: i32, percent: u16) {
    surf.brush_line(x0, y, x1, y, 2, FADED);
    for tick in [MIN_SCALE_PERCENT, 100, MAX_SCALE_PERCENT] {
        let x = slider_x(x0, x1, tick);
        surf.brush_line(x, y - 12, x, y + 12, 2, FADED);
    }
    let thumb = slider_x(x0, x1, percent);
    surf.stamp(thumb, y, 16, BLACK);
}

fn blit_handwriting_left(
    surf: &mut Surface,
    fonts: &FontBook,
    primary: FontId,
    text: &str,
    size: f32,
    x: usize,
    y: usize,
) -> HitRect {
    let line = script::rasterize_line_with(fonts, primary, text, size);
    for row in 0..line.height {
        for col in 0..line.width {
            if line.mask[row * line.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
    HitRect::from_xywh(x as i32, y as i32, line.width as i32, line.height as i32)
}

fn blit_ui_left(
    surf: &mut Surface,
    fonts: &FontBook,
    text: &str,
    size: f32,
    x: usize,
    y: usize,
) -> HitRect {
    let line = script::rasterize_ui_line(fonts, text, size);
    for row in 0..line.height {
        for col in 0..line.width {
            if line.mask[row * line.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
    HitRect::from_xywh(x as i32, y as i32, line.width as i32, line.height as i32)
}

fn draw_frame(surf: &mut Surface, rect: HitRect, thickness: usize, color: u16) {
    let x = rect.x0.max(0) as usize;
    let y = rect.y0.max(0) as usize;
    let width = rect.width().max(0) as usize;
    let height = rect.height().max(0) as usize;
    if width < thickness * 2 || height < thickness * 2 {
        return;
    }
    surf.fill_rect(x, y, width, thickness, color);
    surf.fill_rect(x, y + height - thickness, width, thickness, color);
    surf.fill_rect(x, y, thickness, height, color);
    surf.fill_rect(x + width - thickness, y, thickness, height, color);
}

fn blit_ui_centered(surf: &mut Surface, fonts: &FontBook, text: &str, size: f32, y: usize) {
    let line = script::rasterize_ui_line(fonts, text, size);
    let x = screen_w().saturating_sub(line.width) / 2;
    for row in 0..line.height {
        for col in 0..line.width {
            if line.mask[row * line.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
}

#[cfg(test)]
#[path = "font_settings/tests.rs"]
mod tests;
