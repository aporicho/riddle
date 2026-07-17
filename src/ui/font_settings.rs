//! Paper-native font selection and per-font visual-size calibration.

use crate::fb::{screen_h, screen_w, BBox};
use crate::fonts::{FontBook, FontId, MAX_SCALE_PERCENT, MIN_SCALE_PERCENT};
use crate::script;
use crate::surface::{Surface, BLACK, FADED, WHITE};

const SIDE: usize = 100;
const LIST_TOP: usize = 260;
const ROW_H: usize = 330;
const SLIDER_SIDE: usize = 180;
const SLIDER_Y_OFFSET: usize = 244;
const SLIDER_TOUCH_PAD: i32 = 46;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Select(FontId),
    SetScale(FontId, u16),
    Dismiss,
    Redraw,
}

#[derive(Clone, Copy, Debug)]
struct Row {
    id: FontId,
    y0: i32,
    y1: i32,
    slider_x0: i32,
    slider_x1: i32,
    slider_y: i32,
}

pub struct FontPanel {
    saved: Vec<u8>,
    rows: Vec<Row>,
    stroke: Vec<(i32, i32)>,
}

impl FontPanel {
    pub fn show(surf: &mut Surface, fonts: &FontBook) -> Self {
        let mut panel = Self {
            saved: surf.copy_rect(0, 0, screen_w(), screen_h()),
            rows: Vec::new(),
            stroke: Vec::new(),
        };
        panel.redraw(surf, fonts);
        panel
    }

    pub fn redraw(&mut self, surf: &mut Surface, fonts: &FontBook) {
        surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
        self.rows.clear();
        self.stroke.clear();

        blit_centered(surf, fonts, fonts.selected(), "字体与大小", 82.0, 82);
        blit_centered(
            surf,
            fonts,
            fonts.selected(),
            "点击字体切换 · 拖动横线校准大小 · 点击空白退出",
            40.0,
            screen_h().saturating_sub(105),
        );

        for (index, id) in fonts.available().enumerate() {
            let y0 = LIST_TOP + index * ROW_H;
            let y1 = y0 + ROW_H - 1;
            let marker = if id == fonts.selected() {
                "当前 · "
            } else {
                ""
            };
            let percent = fonts.scale_percent(id);
            let title = format!("{marker}{}  {percent}%", id.display_name());
            blit_left(surf, fonts, id, &title, 58.0, SIDE + 20, y0 + 24);
            blit_left(
                surf,
                fonts,
                id,
                "任务提醒 · 今日阅读 · MagicPaper",
                44.0,
                SIDE + 20,
                y0 + 112,
            );
            let slider_x0 = SLIDER_SIDE as i32;
            let slider_x1 = screen_w().saturating_sub(SLIDER_SIDE) as i32;
            let slider_y = (y0 + SLIDER_Y_OFFSET) as i32;
            draw_slider(surf, slider_x0, slider_x1, slider_y, percent);
            surf.fill_rect(SIDE, y1, screen_w() - SIDE * 2, 2, FADED);
            self.rows.push(Row {
                id,
                y0: y0 as i32,
                y1: y1 as i32,
                slider_x0,
                slider_x1,
                slider_y,
            });
        }
    }

    pub fn pen_point(&mut self, surf: &mut Surface, x: i32, y: i32) -> BBox {
        let mut dirty = BBox::empty();
        let slider = self
            .stroke
            .first()
            .copied()
            .and_then(|first| {
                self.rows.iter().copied().find(|row| {
                    first.0 >= row.slider_x0 - SLIDER_TOUCH_PAD
                        && first.0 <= row.slider_x1 + SLIDER_TOUCH_PAD
                        && (first.1 - row.slider_y).abs() <= SLIDER_TOUCH_PAD
                })
            })
            .or_else(|| {
                self.rows.iter().copied().find(|row| {
                    x >= row.slider_x0 - SLIDER_TOUCH_PAD
                        && x <= row.slider_x1 + SLIDER_TOUCH_PAD
                        && (y - row.slider_y).abs() <= SLIDER_TOUCH_PAD
                })
            });
        self.stroke.push((x, y));
        if let Some(row) = slider {
            let percent = slider_percent(&row, x);
            let top = (row.slider_y - 30).max(0) as usize;
            let left = (row.slider_x0 - 30).max(0) as usize;
            let width = (row.slider_x1 - row.slider_x0 + 60).max(1) as usize;
            surf.fill_rect(left, top, width, 60, WHITE);
            draw_slider(surf, row.slider_x0, row.slider_x1, row.slider_y, percent);
            dirty.add(row.slider_x0, row.slider_y, 32);
            dirty.add(row.slider_x1, row.slider_y, 32);
            return dirty;
        }
        let previous = self.stroke.iter().rev().nth(1).copied();
        if let Some((px, py)) = previous {
            surf.brush_line(px, py, x, y, 3, BLACK);
            dirty.add(px, py, 6);
        } else {
            surf.stamp(x, y, 3, BLACK);
        }
        dirty.add(x, y, 6);
        dirty
    }

    pub fn pen_up(&mut self) -> Option<Action> {
        if self.stroke.is_empty() {
            return None;
        }
        let first = self.stroke[0];
        let (mut x0, mut x1, mut y0, mut y1) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
        for &(x, y) in &self.stroke {
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let last = *self.stroke.last().unwrap();
        self.stroke.clear();
        if let Some(row) = self.rows.iter().find(|row| {
            first.0 >= row.slider_x0 - SLIDER_TOUCH_PAD
                && first.0 <= row.slider_x1 + SLIDER_TOUCH_PAD
                && (first.1 - row.slider_y).abs() <= SLIDER_TOUCH_PAD
        }) {
            return Some(Action::SetScale(row.id, slider_percent(row, last.0)));
        }
        if x1 - x0 > 45 || y1 - y0 > 45 {
            return Some(Action::Redraw);
        }
        if let Some(row) = self
            .rows
            .iter()
            .find(|row| first.1 >= row.y0 && first.1 <= row.y1)
        {
            return Some(Action::Select(row.id));
        }
        Some(Action::Dismiss)
    }

    pub fn dismiss(self, surf: &mut Surface) {
        surf.paste_rect(0, 0, screen_w(), screen_h(), &self.saved);
    }
}

fn slider_percent(row: &Row, x: i32) -> u16 {
    let x = x.clamp(row.slider_x0, row.slider_x1);
    let span = (row.slider_x1 - row.slider_x0).max(1) as i64;
    let range = (MAX_SCALE_PERCENT - MIN_SCALE_PERCENT) as i64;
    (MIN_SCALE_PERCENT as i64 + (x - row.slider_x0) as i64 * range / span) as u16
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

fn blit_left(
    surf: &mut Surface,
    fonts: &FontBook,
    primary: FontId,
    text: &str,
    size: f32,
    x: usize,
    y: usize,
) {
    let line = script::rasterize_line_with(fonts, primary, text, size);
    for row in 0..line.height {
        for col in 0..line.width {
            if line.mask[row * line.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
}

fn blit_centered(
    surf: &mut Surface,
    fonts: &FontBook,
    primary: FontId,
    text: &str,
    size: f32,
    y: usize,
) {
    let line = script::rasterize_line_with(fonts, primary, text, size);
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
mod tests {
    use super::*;

    fn panel() -> FontPanel {
        FontPanel {
            saved: Vec::new(),
            rows: vec![
                Row {
                    id: FontId::ChenYuluoyan,
                    y0: 300,
                    y1: 499,
                    slider_x0: 180,
                    slider_x1: 1400,
                    slider_y: 430,
                },
                Row {
                    id: FontId::Farstar851,
                    y0: 500,
                    y1: 699,
                    slider_x0: 180,
                    slider_x1: 1400,
                    slider_y: 630,
                },
            ],
            stroke: Vec::new(),
        }
    }

    #[test]
    fn short_row_tap_selects_and_blank_tap_dismisses() {
        let mut panel = panel();
        panel.stroke = vec![(500, 550), (502, 551)];
        assert_eq!(panel.pen_up(), Some(Action::Select(FontId::Farstar851)));
        panel.stroke = vec![(500, 900), (501, 902)];
        assert_eq!(panel.pen_up(), Some(Action::Dismiss));
    }

    #[test]
    fn long_stroke_never_selects() {
        let mut panel = panel();
        panel.stroke = vec![(200, 550), (600, 550)];
        assert_eq!(panel.pen_up(), Some(Action::Redraw));
    }

    #[test]
    fn slider_tap_and_drag_map_to_calibration_range() {
        let mut panel = panel();
        panel.stroke = vec![(180, 630), (180, 630)];
        assert_eq!(
            panel.pen_up(),
            Some(Action::SetScale(FontId::Farstar851, MIN_SCALE_PERCENT))
        );
        panel.stroke = vec![(600, 630), (1400, 630)];
        assert_eq!(
            panel.pen_up(),
            Some(Action::SetScale(FontId::Farstar851, MAX_SCALE_PERCENT))
        );
    }
}
