//! The paper-native recurring-task list.
//!
//! It is deliberately local: the oracle only recognizes the handwritten
//! `任务` / `task` entry word, while rendering and strike-to-delete happen on
//! the device without another network request.

use crate::fb::{screen_h, screen_w, BBox};
use crate::fonts::FontBook;
use crate::script;
use crate::surface::{Surface, BLACK, FADED, WHITE};

const SIDE: usize = 80;
const LIST_TOP: usize = 270;
const LIST_BOTTOM_PAD: usize = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Delete(usize),
    Toggle(usize),
    Dismiss,
    Redraw,
}

#[derive(Clone, Copy, Debug)]
struct Row {
    number: usize,
    y0: i32,
    y1: i32,
    toggle_box: Option<(i32, i32, i32, i32)>,
}

pub struct PaperList {
    saved: Vec<u8>,
    rows: Vec<Row>,
    stroke: Vec<(i32, i32)>,
}

impl PaperList {
    pub fn show(
        surf: &mut Surface,
        font: &FontBook,
        title: &str,
        empty_text: &str,
        footer: &str,
        entries: &[String],
        enabled: Option<&[bool]>,
    ) -> Self {
        let saved = surf.copy_rect(0, 0, screen_w(), screen_h());
        let mut panel = Self {
            saved,
            rows: Vec::new(),
            stroke: Vec::new(),
        };
        panel.redraw(surf, font, title, empty_text, footer, entries, enabled);
        panel
    }

    pub fn redraw(
        &mut self,
        surf: &mut Surface,
        font: &FontBook,
        title: &str,
        empty_text: &str,
        footer: &str,
        entries: &[String],
        enabled: Option<&[bool]>,
    ) {
        surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
        self.rows.clear();
        self.stroke.clear();

        blit_centered(surf, font, title, 88.0, 90);
        blit_centered(surf, font, footer, 42.0, screen_h().saturating_sub(115));

        if entries.is_empty() {
            blit_centered(surf, font, empty_text, 68.0, LIST_TOP + 300);
            return;
        }

        let available = screen_h().saturating_sub(LIST_TOP + LIST_BOTTOM_PAD);
        let row_h = (available / entries.len()).clamp(78, 170);
        let text_px = ((row_h as f32) * 0.36).clamp(40.0, 58.0);
        let has_toggles = enabled.is_some();
        let max_width = screen_w().saturating_sub(SIDE * 2 + if has_toggles { 120 } else { 24 });
        for (index, entry) in entries.iter().enumerate() {
            let y0 = LIST_TOP + index * row_h;
            let y1 = y0 + row_h - 1;
            let text = fit_line(font, entry, text_px, max_width);
            let text_y = y0 + (row_h.saturating_sub(text_px as usize)) / 2;
            blit_left(surf, font, &text, text_px, SIDE + 12, text_y);
            let toggle_box = enabled.and_then(|states| states.get(index)).map(|active| {
                let size = 54usize.min(row_h.saturating_sub(20));
                let x = screen_w().saturating_sub(SIDE + size + 16);
                let y = y0 + row_h.saturating_sub(size) / 2;
                draw_status_box(surf, x, y, size, *active);
                (x as i32, y as i32, (x + size) as i32, (y + size) as i32)
            });
            surf.fill_rect(SIDE, y1, screen_w() - SIDE * 2, 2, FADED);
            self.rows.push(Row {
                number: index + 1,
                y0: y0 as i32,
                y1: y1 as i32,
                toggle_box,
            });
        }
    }

    /// Draw a live strike and retain its geometry for classification on lift.
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
        let mut x0 = i32::MAX;
        let mut x1 = i32::MIN;
        let mut y0 = i32::MAX;
        let mut y1 = i32::MIN;
        for &(x, y) in &self.stroke {
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let first = self.stroke[0];
        let last = *self.stroke.last().unwrap();
        self.stroke.clear();

        let x_span = x1 - x0;
        let y_span = y1 - y0;
        let horizontal = x_span >= 140
            && x_span >= y_span.saturating_mul(2)
            && (last.0 - first.0).abs() >= (last.1 - first.1).abs().saturating_mul(2);
        if horizontal {
            let center_y = (y0 + y1) / 2;
            if let Some(row) = self
                .rows
                .iter()
                .find(|row| center_y >= row.y0 && center_y <= row.y1)
            {
                return Some(Action::Delete(row.number));
            }
        }

        // A small contact outside every task row is the blank-space exit.
        if x_span <= 45 && y_span <= 45 {
            if let Some(row) = self.rows.iter().find(|row| {
                row.toggle_box.is_some_and(|(left, top, right, bottom)| {
                    first.0 >= left && first.0 <= right && first.1 >= top && first.1 <= bottom
                })
            }) {
                return Some(Action::Toggle(row.number));
            }
            let inside_row = self
                .rows
                .iter()
                .any(|row| first.1 >= row.y0 && first.1 <= row.y1);
            if !inside_row {
                return Some(Action::Dismiss);
            }
        }
        Some(Action::Redraw)
    }

    pub fn dismiss(self, surf: &mut Surface) {
        surf.paste_rect(0, 0, screen_w(), screen_h(), &self.saved);
    }
}

fn draw_status_box(surf: &mut Surface, x: usize, y: usize, size: usize, active: bool) {
    let thickness = 3;
    surf.fill_rect(x, y, size, thickness, BLACK);
    surf.fill_rect(x, y + size - thickness, size, thickness, BLACK);
    surf.fill_rect(x, y, thickness, size, BLACK);
    surf.fill_rect(x + size - thickness, y, thickness, size, BLACK);
    if active {
        surf.brush_line(
            (x + size / 5) as i32,
            (y + size / 2) as i32,
            (x + size * 2 / 5) as i32,
            (y + size * 4 / 5) as i32,
            3,
            BLACK,
        );
        surf.brush_line(
            (x + size * 2 / 5) as i32,
            (y + size * 4 / 5) as i32,
            (x + size * 4 / 5) as i32,
            (y + size / 5) as i32,
            3,
            BLACK,
        );
    } else {
        surf.brush_line(
            (x + size / 4) as i32,
            (y + size / 4) as i32,
            (x + size * 3 / 4) as i32,
            (y + size * 3 / 4) as i32,
            3,
            BLACK,
        );
        surf.brush_line(
            (x + size * 3 / 4) as i32,
            (y + size / 4) as i32,
            (x + size / 4) as i32,
            (y + size * 3 / 4) as i32,
            3,
            BLACK,
        );
    }
}

fn fit_line(font: &FontBook, text: &str, size: f32, max_width: usize) -> String {
    if script::measure(font, text, size) as usize <= max_width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate = format!("{}…", chars.iter().collect::<String>());
        if script::measure(font, &candidate, size) as usize <= max_width {
            return candidate;
        }
    }
    "…".into()
}

fn blit_left(surf: &mut Surface, font: &FontBook, text: &str, size: f32, x: usize, y: usize) {
    let line = script::rasterize_line(font, text, size);
    for row in 0..line.height {
        for col in 0..line.width {
            if line.mask[row * line.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
}

fn blit_centered(surf: &mut Surface, font: &FontBook, text: &str, size: f32, y: usize) {
    let line = script::rasterize_line(font, text, size);
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

    fn panel_with_rows() -> PaperList {
        PaperList {
            saved: Vec::new(),
            rows: vec![
                Row {
                    number: 1,
                    y0: 300,
                    y1: 469,
                    toggle_box: Some((1300, 330, 1360, 390)),
                },
                Row {
                    number: 2,
                    y0: 470,
                    y1: 639,
                    toggle_box: Some((1300, 520, 1360, 580)),
                },
            ],
            stroke: Vec::new(),
        }
    }

    #[test]
    fn horizontal_strike_selects_its_row() {
        let mut panel = panel_with_rows();
        panel.stroke = vec![(200, 550), (500, 548), (900, 553)];
        assert_eq!(panel.pen_up(), Some(Action::Delete(2)));
    }

    #[test]
    fn blank_tap_dismisses_but_row_tap_does_not() {
        let mut panel = panel_with_rows();
        panel.stroke = vec![(800, 900), (802, 901)];
        assert_eq!(panel.pen_up(), Some(Action::Dismiss));
        panel.stroke = vec![(800, 350), (802, 351)];
        assert_eq!(panel.pen_up(), Some(Action::Redraw));
    }

    #[test]
    fn status_box_tap_toggles_its_row() {
        let mut panel = panel_with_rows();
        panel.stroke = vec![(1330, 550), (1332, 551)];
        assert_eq!(panel.pen_up(), Some(Action::Toggle(2)));
    }

    #[test]
    fn vertical_or_short_strokes_do_not_delete() {
        let mut panel = panel_with_rows();
        panel.stroke = vec![(500, 490), (505, 620)];
        assert_eq!(panel.pen_up(), Some(Action::Redraw));
        panel.stroke = vec![(500, 550), (590, 550)];
        assert_eq!(panel.pen_up(), Some(Action::Redraw));
    }
}
