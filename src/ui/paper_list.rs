//! Shared paper-native task, TODO, and history list component.
//!
//! It is deliberately local: the oracle only recognizes the handwritten
//! `任务` / `task` entry word, while rendering and strike-to-delete happen on
//! the device without another network request.

use crate::fb::{screen_h, screen_w};
use crate::fonts::FontBook;
use crate::surface::{Surface, FADED, WHITE};

use super::pointer::{
    draw_clipped_line, invert_mono, Gesture, GesturePolicy, HitRect, Point, PointerTool,
};

mod geometry;
pub(in crate::ui) mod render;
use geometry::segment_intersects_rect;
use render::{blit_centered, blit_left, draw_frame, draw_status_box, fit_line};

const SIDE: usize = 80;
const LIST_TOP: usize = 270;
const LIST_BOTTOM_PAD: usize = 180;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Delete(usize),
    Toggle(usize),
    Select(usize),
    Dismiss,
    Redraw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Text(usize),
    Toggle(usize),
    SelectCard(usize),
    Blank,
}

/// Component-owned transient feedback. Runtime only snapshots [`Self::rect`]
/// and delegates drawing/release semantics back to this component, so hit
/// geometry never has to be duplicated outside the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    Press {
        tool: PointerTool,
        target: Hit,
        rect: HitRect,
        start: Point,
        pressed: bool,
    },
    Strike {
        row: usize,
        card: HitRect,
        text: HitRect,
        start: Point,
        line_to: Option<Point>,
    },
}

impl Preview {
    pub const fn rect(self) -> HitRect {
        match self {
            Self::Press { rect, .. } => rect,
            Self::Strike { card, .. } => card,
        }
    }

    pub const fn is_visible(self) -> bool {
        matches!(
            self,
            Self::Press { pressed: true, .. }
                | Self::Strike {
                    line_to: Some(_),
                    ..
                }
        )
    }

    /// Update only the visual state. Returns true when the save-under must be
    /// restored and this preview rendered again.
    pub fn update(&mut self, point: Point) -> bool {
        match self {
            Self::Press { rect, pressed, .. } => {
                let next = rect.contains(point);
                let changed = *pressed != next;
                *pressed = next;
                changed
            }
            Self::Strike {
                text,
                start,
                line_to,
                ..
            } => {
                let dx = (point.x - start.x).abs();
                let dy = (point.y - start.y).abs();
                const PREVIEW_DISTANCE_PX: i32 = 32;
                let dominance = GesturePolicy::default().pen.axis_dominance;
                let next = (dx >= PREVIEW_DISTANCE_PX
                    && dx >= dy.saturating_mul(dominance)
                    && segment_intersects_rect(*start, point, *text))
                .then_some(point);
                let changed = *line_to != next;
                *line_to = next;
                changed
            }
        }
    }

    pub fn render(self, surface: &mut Surface) {
        match self {
            Self::Press {
                rect,
                pressed: true,
                ..
            } => invert_mono(surface, rect),
            Self::Strike {
                card,
                start,
                line_to: Some(to),
                ..
            } => draw_clipped_line(surface, start, to, card, 3),
            _ => {}
        }
    }

    /// Convert a completed preview back into the component's existing
    /// gesture/action path. A released-outside press becomes a zero-distance
    /// no-op swipe, while a slider-like accidental drag can never dismiss the
    /// page underneath.
    pub fn release_gesture(self, end: Point, classified: Option<Gesture>) -> Gesture {
        match self {
            Self::Press {
                tool,
                pressed: true,
                ..
            } => Gesture::Tap { tool, at: end },
            Self::Strike {
                row: _,
                card,
                text,
                start,
                ..
            } => match classified {
                Some(
                    gesture @ Gesture::Strike {
                        tool: PointerTool::Pen,
                        from,
                        to,
                        ..
                    },
                ) if from == start
                    && card.contains(from)
                    && segment_intersects_rect(from, to, text) =>
                {
                    gesture
                }
                Some(Gesture::Tap { tool, .. }) => Gesture::Tap { tool, at: end },
                Some(gesture @ Gesture::Swipe { .. }) => gesture,
                _ => no_op_gesture(PointerTool::Pen, start),
            },
            Self::Press { tool, start, .. } => no_op_gesture(tool, start),
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

pub struct Content<'a> {
    pub title: &'a str,
    pub empty_text: &'a str,
    pub footer: &'a str,
    pub entries: &'a [String],
    pub enabled: Option<&'a [bool]>,
}

#[derive(Clone, Copy, Debug)]
struct Row {
    number: usize,
    card: HitRect,
    text: HitRect,
    toggle_box: Option<HitRect>,
}

pub struct PaperList {
    rows: Vec<Row>,
    selectable: bool,
    page: usize,
    page_size: usize,
    total_rows: usize,
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
        Self::show_with_mode(
            surf,
            font,
            Content {
                title,
                empty_text,
                footer,
                entries,
                enabled,
            },
            false,
        )
    }

    /// A non-destructive list whose rows open an item on a small pen tap.
    pub fn show_selectable(
        surf: &mut Surface,
        font: &FontBook,
        title: &str,
        empty_text: &str,
        footer: &str,
        entries: &[String],
    ) -> Self {
        Self::show_with_mode(
            surf,
            font,
            Content {
                title,
                empty_text,
                footer,
                entries,
                enabled: None,
            },
            true,
        )
    }

    fn show_with_mode(
        surf: &mut Surface,
        font: &FontBook,
        content: Content<'_>,
        selectable: bool,
    ) -> Self {
        let mut panel = Self {
            rows: Vec::new(),
            selectable,
            page: 0,
            page_size: 1,
            total_rows: 0,
        };
        panel.redraw(surf, font, content);
        panel
    }

    pub fn redraw(&mut self, surf: &mut Surface, font: &FontBook, content: Content<'_>) {
        let Content {
            title,
            empty_text,
            footer,
            entries,
            enabled,
        } = content;
        surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
        self.rows.clear();

        blit_centered(surf, font, title, 88.0, 90);
        if entries.is_empty() {
            blit_centered(surf, font, footer, 42.0, screen_h().saturating_sub(115));
            self.page = 0;
            self.page_size = 1;
            self.total_rows = 0;
            blit_centered(surf, font, empty_text, 68.0, LIST_TOP + 300);
            return;
        }

        let available = screen_h().saturating_sub(LIST_TOP + LIST_BOTTOM_PAD);
        self.page_size = (available / 78).max(1);
        self.total_rows = entries.len();
        let page_count = entries.len().div_ceil(self.page_size);
        self.page = self.page.min(page_count.saturating_sub(1));
        let start = self.page * self.page_size;
        let end = (start + self.page_size).min(entries.len());
        let visible = &entries[start..end];
        let footer = if page_count > 1 {
            format!("{footer} · 上下划翻页 · {}/{}", self.page + 1, page_count)
        } else {
            footer.to_string()
        };
        blit_centered(surf, font, &footer, 42.0, screen_h().saturating_sub(115));
        let row_h = (available / visible.len()).clamp(78, 170);
        let text_px = ((row_h as f32) * 0.36).clamp(40.0, 58.0);
        let has_toggles = enabled.is_some();
        let max_width = screen_w().saturating_sub(SIDE * 2 + if has_toggles { 120 } else { 24 });
        for (visible_index, entry) in visible.iter().enumerate() {
            let index = start + visible_index;
            let y0 = LIST_TOP + visible_index * row_h;
            let y1 = y0 + row_h - 1;
            let text = fit_line(font, entry, text_px, max_width);
            let card = HitRect::from_xywh(
                SIDE as i32,
                (y0 + 5) as i32,
                screen_w().saturating_sub(SIDE * 2) as i32,
                row_h.saturating_sub(10) as i32,
            );
            if self.selectable {
                draw_frame(surf, card, 2, FADED);
            }
            let text_y = y0 + (row_h.saturating_sub(text_px as usize)) / 2;
            let text_x = SIDE + if self.selectable { 28 } else { 12 };
            let text_rect = blit_left(surf, font, &text, text_px, text_x, text_y);
            let toggle_box = enabled.and_then(|states| states.get(index)).map(|active| {
                let size = 54usize.min(row_h.saturating_sub(20));
                let x = screen_w().saturating_sub(SIDE + size + 16);
                let y = y0 + row_h.saturating_sub(size) / 2;
                draw_status_box(surf, x, y, size, *active);
                HitRect::from_xywh(x as i32, y as i32, size as i32, size as i32)
            });
            if !self.selectable {
                surf.fill_rect(SIDE, y1, screen_w() - SIDE * 2, 2, FADED);
            }
            self.rows.push(Row {
                number: index + 1,
                card,
                text: text_rect,
                toggle_box,
            });
        }
    }

    pub fn hit_test(&self, point: Point) -> Hit {
        if let Some(row) = self.rows.iter().find(|row| {
            row.toggle_box
                .is_some_and(|toggle_box| toggle_box.contains(point))
        }) {
            return Hit::Toggle(row.number);
        }
        if self.selectable {
            return self
                .rows
                .iter()
                .find(|row| row.card.contains(point))
                .map_or(Hit::Blank, |row| Hit::SelectCard(row.number));
        }
        self.rows
            .iter()
            .find(|row| row.text.contains(point))
            .map_or(Hit::Blank, |row| Hit::Text(row.number))
    }

    pub fn begin_preview(&self, tool: PointerTool, point: Point) -> Option<Preview> {
        let target = self.hit_test(point);
        match target {
            Hit::Toggle(number) => self
                .rows
                .iter()
                .find(|row| row.number == number)
                .and_then(|row| row.toggle_box)
                .map(|rect| Preview::Press {
                    tool,
                    target,
                    rect,
                    start: point,
                    pressed: true,
                }),
            Hit::SelectCard(number) => {
                self.rows
                    .iter()
                    .find(|row| row.number == number)
                    .map(|row| Preview::Press {
                        tool,
                        target,
                        rect: row.card,
                        start: point,
                        pressed: true,
                    })
            }
            _ if tool == PointerTool::Pen && !self.selectable => self
                .rows
                .iter()
                .find(|row| row.card.contains(point))
                .map(|row| Preview::Strike {
                    row: row.number,
                    card: row.card,
                    text: row.text,
                    start: point,
                    line_to: None,
                }),
            _ => None,
        }
    }

    /// Resolve a tool-aware gesture without drawing it. Destructive strikes
    /// are accepted only when the caller classified the contact as pen input.
    pub fn interact(&mut self, gesture: Gesture) -> Option<Action> {
        match gesture {
            Gesture::Tap { at, .. } => Some(match self.hit_test(at) {
                Hit::Toggle(number) => Action::Toggle(number),
                Hit::SelectCard(number) => Action::Select(number),
                Hit::Text(_) => Action::Redraw,
                Hit::Blank => Action::Dismiss,
            }),
            Gesture::Strike {
                tool: PointerTool::Pen,
                from,
                to,
                bounds,
                ..
            } if !self.selectable => Some(
                self.rows
                    .iter()
                    .find(|row| {
                        row.card.contains(from)
                            && row.text.intersects(bounds)
                            && segment_intersects_rect(from, to, row.text)
                    })
                    .map_or(Action::Redraw, |row| Action::Delete(row.number)),
            ),
            Gesture::Swipe { from, to, .. } if (to.y - from.y).abs() > (to.x - from.x).abs() => {
                let page_count = self.total_rows.div_ceil(self.page_size.max(1));
                if to.y < from.y {
                    self.page = (self.page + 1).min(page_count.saturating_sub(1));
                } else {
                    self.page = self.page.saturating_sub(1);
                }
                Some(Action::Redraw)
            }
            _ => Some(Action::Redraw),
        }
    }

    pub fn refresh_region(&self) -> crate::fb::BBox {
        crate::fb::BBox {
            x0: SIDE as i32,
            y0: LIST_TOP as i32,
            x1: screen_w().saturating_sub(SIDE + 1) as i32,
            y1: screen_h().saturating_sub(48) as i32,
        }
    }
}

#[cfg(test)]
#[path = "paper_list/tests.rs"]
mod tests;
