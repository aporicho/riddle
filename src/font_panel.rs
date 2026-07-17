//! Paper-native font picker. A pen tap previews/selects a row immediately;
//! tapping blank paper leaves the picker and restores the previous page.

use crate::fb::{screen_h, screen_w, BBox};
use crate::fonts::{FontBook, FontId};
use crate::script;
use crate::surface::{Surface, BLACK, FADED, WHITE};

const SIDE: usize = 100;
const LIST_TOP: usize = 330;
const ROW_H: usize = 250;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Select(FontId),
    Dismiss,
    Redraw,
}

#[derive(Clone, Copy, Debug)]
struct Row {
    id: FontId,
    y0: i32,
    y1: i32,
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

        blit_centered(surf, fonts, fonts.selected(), "字体", 92.0, 105);
        blit_centered(
            surf,
            fonts,
            fonts.selected(),
            "点击字体立即切换 · 点击空白处退出",
            44.0,
            screen_h().saturating_sub(125),
        );

        for (index, id) in fonts.available().enumerate() {
            let y0 = LIST_TOP + index * ROW_H;
            let y1 = y0 + ROW_H - 1;
            let marker = if id == fonts.selected() {
                "当前 · "
            } else {
                ""
            };
            let title = format!("{marker}{}", id.display_name());
            blit_left(surf, fonts, id, &title, 66.0, SIDE + 20, y0 + 36);
            blit_left(
                surf,
                fonts,
                id,
                "任务提醒 · 今日阅读 · MagicPaper",
                50.0,
                SIDE + 20,
                y0 + 128,
            );
            surf.fill_rect(SIDE, y1, screen_w() - SIDE * 2, 2, FADED);
            self.rows.push(Row {
                id,
                y0: y0 as i32,
                y1: y1 as i32,
            });
        }
    }

    pub fn pen_point(&mut self, surf: &mut Surface, x: i32, y: i32) -> BBox {
        let mut dirty = BBox::empty();
        if let Some(&(px, py)) = self.stroke.last() {
            surf.brush_line(px, py, x, y, 3, BLACK);
            dirty.add(px, py, 6);
        } else {
            surf.stamp(x, y, 3, BLACK);
        }
        dirty.add(x, y, 6);
        self.stroke.push((x, y));
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
        self.stroke.clear();
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
                },
                Row {
                    id: FontId::Farstar851,
                    y0: 500,
                    y1: 699,
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
}
