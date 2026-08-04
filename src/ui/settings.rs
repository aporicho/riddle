//! Paper-native controls for immediately applicable experience preferences.

use crate::fb::{screen_h, screen_w};
use crate::fonts::FontBook;
use crate::preferences::{
    CleanupStrength, PreferenceValues, ANSWER_DWELL_STEP_PERCENT, CLEANUP_PADDING_STEP_PX,
    MAX_ANSWER_DWELL_PERCENT, MAX_CLEANUP_PADDING_PX, MAX_FULL_REFRESH_INTERVAL,
    MIN_ANSWER_DWELL_PERCENT, MIN_CLEANUP_PADDING_PX, MIN_FULL_REFRESH_INTERVAL,
};
use crate::surface::{Surface, FADED, WHITE};

use super::paper_list::render::{blit_centered, blit_left, draw_frame, fit_line};
use super::pointer::{invert_mono, Gesture, HitRect, Point, PointerTool};

const SIDE: usize = 72;
const LIST_TOP: usize = 220;
const ROW_H: usize = 188;
const SLIDER_X0: i32 = 170;
const SLIDER_SIDE: usize = 170;
const SLIDER_Y_OFFSET: usize = 130;
const SLIDER_HALF_HEIGHT: i32 = 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Setting {
    CleanupStrength,
    CleanupPadding,
    FullRefreshInterval,
    AnswerDwell,
    PiAgent,
    Fonts,
    RefreshNow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SetCleanupStrength(CleanupStrength),
    SetCleanupPadding(u8),
    SetFullRefreshInterval(u8),
    SetAnswerDwell(u16),
    OpenPiAgent,
    OpenFonts,
    RefreshNow,
    Dismiss,
    Redraw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Card(Setting),
    Slider(Setting),
    Blank,
}

#[derive(Clone, Copy, Debug)]
struct Row {
    setting: Setting,
    y0: usize,
    card: HitRect,
    slider: Option<HitRect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    Press {
        tool: PointerTool,
        setting: Setting,
        card: HitRect,
        start: Point,
        pressed: bool,
    },
    Slider {
        tool: PointerTool,
        setting: Setting,
        card: HitRect,
        control: HitRect,
        y0: usize,
        start: Point,
        draft: PreferenceValues,
    },
}

impl Preview {
    pub const fn rect(self) -> HitRect {
        match self {
            Self::Press { card, .. } | Self::Slider { card, .. } => card,
        }
    }

    pub const fn is_visible(self) -> bool {
        matches!(
            self,
            Self::Press { pressed: true, .. } | Self::Slider { .. }
        )
    }

    pub fn update(&mut self, point: Point) -> bool {
        match self {
            Self::Press { card, pressed, .. } => {
                let next = card.contains(point);
                let changed = *pressed != next;
                *pressed = next;
                changed
            }
            Self::Slider {
                setting,
                control,
                draft,
                ..
            } => {
                let previous = *draft;
                apply_slider(*setting, draft, slider_x(*control, point.x));
                previous != *draft
            }
        }
    }

    pub fn render(self, surface: &mut Surface, fonts: &FontBook) {
        match self {
            Self::Press {
                card,
                pressed: true,
                ..
            } => invert_mono(surface, card),
            Self::Slider {
                setting, y0, draft, ..
            } => {
                let _ = draw_row(surface, fonts, setting, y0, draft);
            }
            _ => {}
        }
    }

    pub fn release_gesture(self, end: Point) -> Gesture {
        match self {
            Self::Press {
                tool,
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
            Self::Press { tool, start, .. } => no_op_gesture(tool, start),
        }
    }
}

pub struct SettingsPanel {
    rows: Vec<Row>,
    values: PreferenceValues,
}

impl SettingsPanel {
    pub fn show(surf: &mut Surface, fonts: &FontBook, values: PreferenceValues) -> Self {
        let mut panel = Self {
            rows: Vec::new(),
            values,
        };
        panel.redraw(surf, fonts, values);
        panel
    }

    pub fn redraw(&mut self, surf: &mut Surface, fonts: &FontBook, values: PreferenceValues) {
        self.values = values;
        self.rows.clear();
        surf.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
        blit_centered(surf, fonts, "设置", 82.0, 78);
        blit_centered(
            surf,
            fonts,
            "点选或拖动调整 · 点击空白退出",
            40.0,
            screen_h().saturating_sub(102),
        );
        for (index, setting) in settings().into_iter().enumerate() {
            let y0 = LIST_TOP + index * ROW_H;
            let (card, slider) = draw_row(surf, fonts, setting, y0, values);
            self.rows.push(Row {
                setting,
                y0,
                card,
                slider,
            });
        }
    }

    fn hit_test(&self, point: Point) -> Hit {
        if let Some(row) = self
            .rows
            .iter()
            .find(|row| row.slider.is_some_and(|slider| slider.contains(point)))
        {
            return Hit::Slider(row.setting);
        }
        self.rows
            .iter()
            .find(|row| row.card.contains(point))
            .map_or(Hit::Blank, |row| Hit::Card(row.setting))
    }

    pub fn begin_preview(&self, tool: PointerTool, point: Point) -> Option<Preview> {
        match self.hit_test(point) {
            Hit::Slider(setting) => {
                let row = self.rows.iter().find(|row| row.setting == setting)?;
                Some(Preview::Slider {
                    tool,
                    setting,
                    card: row.card,
                    control: row.slider?,
                    y0: row.y0,
                    start: point,
                    draft: self.values,
                })
            }
            Hit::Card(setting) => {
                let row = self.rows.iter().find(|row| row.setting == setting)?;
                Some(Preview::Press {
                    tool,
                    setting,
                    card: row.card,
                    start: point,
                    pressed: true,
                })
            }
            Hit::Blank => None,
        }
    }

    pub fn interact(&self, gesture: Gesture) -> Option<Action> {
        match gesture {
            Gesture::Tap { at, .. } => Some(match self.hit_test(at) {
                Hit::Slider(setting) => self.slider_action(setting, at.x),
                Hit::Card(Setting::CleanupStrength) => {
                    Action::SetCleanupStrength(self.values.cleanup_strength.toggled())
                }
                Hit::Card(Setting::PiAgent) => Action::OpenPiAgent,
                Hit::Card(Setting::Fonts) => Action::OpenFonts,
                Hit::Card(Setting::RefreshNow) => Action::RefreshNow,
                Hit::Card(_) => Action::Redraw,
                Hit::Blank => Action::Dismiss,
            }),
            Gesture::Swipe { from, to, .. } | Gesture::Strike { from, to, .. } => {
                let row = self
                    .rows
                    .iter()
                    .find(|row| row.slider.is_some_and(|control| control.contains(from)));
                Some(row.map_or(Action::Redraw, |row| self.slider_action(row.setting, to.x)))
            }
        }
    }

    fn slider_action(&self, setting: Setting, x: i32) -> Action {
        let Some(control) = self
            .rows
            .iter()
            .find(|row| row.setting == setting)
            .and_then(|row| row.slider)
        else {
            return Action::Redraw;
        };
        let mut values = self.values;
        apply_slider(setting, &mut values, slider_x(control, x));
        match setting {
            Setting::CleanupPadding => Action::SetCleanupPadding(values.cleanup_padding_px),
            Setting::FullRefreshInterval => {
                Action::SetFullRefreshInterval(values.full_refresh_every_replies)
            }
            Setting::AnswerDwell => Action::SetAnswerDwell(values.answer_dwell_percent),
            _ => Action::Redraw,
        }
    }
}

const fn settings() -> [Setting; 7] {
    [
        Setting::CleanupStrength,
        Setting::CleanupPadding,
        Setting::FullRefreshInterval,
        Setting::AnswerDwell,
        Setting::PiAgent,
        Setting::Fonts,
        Setting::RefreshNow,
    ]
}

fn draw_row(
    surf: &mut Surface,
    fonts: &FontBook,
    setting: Setting,
    y0: usize,
    values: PreferenceValues,
) -> (HitRect, Option<HitRect>) {
    let card = HitRect::from_xywh(
        SIDE as i32,
        y0 as i32,
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
    let title = match setting {
        Setting::CleanupStrength => {
            format!("局部清理 · {}", values.cleanup_strength.label())
        }
        Setting::CleanupPadding => format!("清理边距 · {} px", values.cleanup_padding_px),
        Setting::FullRefreshInterval => match values.full_refresh_every_replies {
            0 => "自动全刷 · 关闭".into(),
            count => format!("自动全刷 · 每 {count} 次回答"),
        },
        Setting::AnswerDwell => format!("回答停留 · {}%", values.answer_dwell_percent),
        Setting::PiAgent => "Pi 智能体 · 模型与工具".into(),
        Setting::Fonts => format!("字体与大小 · {}", fonts.selected().display_name()),
        Setting::RefreshNow => "立即全刷".into(),
    };
    let title_y = if has_slider(setting) {
        y0 + 22
    } else {
        y0 + 60
    };
    let title = fit_line(
        fonts,
        &title,
        48.0,
        screen_w().saturating_sub(SIDE * 2 + 48),
    );
    blit_left(surf, fonts, &title, 48.0, SIDE + 24, title_y);
    if !has_slider(setting) {
        return (card, None);
    }
    let x1 = screen_w().saturating_sub(SLIDER_SIDE) as i32;
    let y = (y0 + SLIDER_Y_OFFSET) as i32;
    let control = HitRect::from_xywh(
        SLIDER_X0 - 24,
        y - SLIDER_HALF_HEIGHT,
        x1 - SLIDER_X0 + 48,
        SLIDER_HALF_HEIGHT * 2,
    );
    draw_frame(surf, control, 2, FADED);
    draw_slider(surf, SLIDER_X0, x1, y, slider_spec(setting, values));
    (card, Some(control))
}

fn has_slider(setting: Setting) -> bool {
    matches!(
        setting,
        Setting::CleanupPadding | Setting::FullRefreshInterval | Setting::AnswerDwell
    )
}

#[derive(Clone, Copy)]
struct SliderSpec {
    value: u16,
    min: u16,
    max: u16,
    ticks: [u16; 3],
}

fn slider_spec(setting: Setting, values: PreferenceValues) -> SliderSpec {
    match setting {
        Setting::CleanupPadding => SliderSpec {
            value: values.cleanup_padding_px as u16,
            min: MIN_CLEANUP_PADDING_PX as u16,
            max: MAX_CLEANUP_PADDING_PX as u16,
            ticks: [0, 16, 32],
        },
        Setting::FullRefreshInterval => SliderSpec {
            value: values.full_refresh_every_replies as u16,
            min: MIN_FULL_REFRESH_INTERVAL as u16,
            max: MAX_FULL_REFRESH_INTERVAL as u16,
            ticks: [0, 5, 10],
        },
        Setting::AnswerDwell => SliderSpec {
            value: values.answer_dwell_percent,
            min: MIN_ANSWER_DWELL_PERCENT,
            max: MAX_ANSWER_DWELL_PERCENT,
            ticks: [50, 100, 200],
        },
        _ => SliderSpec {
            value: 0,
            min: 0,
            max: 1,
            ticks: [0, 0, 1],
        },
    }
}

fn slider_x(control: HitRect, x: i32) -> i32 {
    x.clamp(control.x0 + 24, control.x1 - 24)
}

fn apply_slider(setting: Setting, values: &mut PreferenceValues, x: i32) {
    let x1 = screen_w().saturating_sub(SLIDER_SIDE) as i32;
    match setting {
        Setting::CleanupPadding => {
            values.cleanup_padding_px = slider_value(
                x,
                SLIDER_X0,
                x1,
                MIN_CLEANUP_PADDING_PX as u16,
                MAX_CLEANUP_PADDING_PX as u16,
                CLEANUP_PADDING_STEP_PX as u16,
            ) as u8;
        }
        Setting::FullRefreshInterval => {
            values.full_refresh_every_replies = slider_value(
                x,
                SLIDER_X0,
                x1,
                MIN_FULL_REFRESH_INTERVAL as u16,
                MAX_FULL_REFRESH_INTERVAL as u16,
                1,
            ) as u8;
        }
        Setting::AnswerDwell => {
            values.answer_dwell_percent = slider_value(
                x,
                SLIDER_X0,
                x1,
                MIN_ANSWER_DWELL_PERCENT,
                MAX_ANSWER_DWELL_PERCENT,
                ANSWER_DWELL_STEP_PERCENT,
            );
        }
        _ => {}
    }
}

fn slider_value(x: i32, x0: i32, x1: i32, min: u16, max: u16, step: u16) -> u16 {
    let span = (x1 - x0).max(1) as i64;
    let steps = (max - min) / step;
    let offset = (x.clamp(x0, x1) - x0) as i64;
    let selected = (offset * steps as i64 + span / 2) / span;
    min + selected as u16 * step
}

fn slider_position(x0: i32, x1: i32, value: u16, min: u16, max: u16) -> i32 {
    let range = (max - min).max(1) as i64;
    x0 + (x1 - x0) * (value.saturating_sub(min) as i32) / range as i32
}

fn draw_slider(surf: &mut Surface, x0: i32, x1: i32, y: i32, spec: SliderSpec) {
    surf.brush_line(x0, y, x1, y, 2, FADED);
    for tick in spec.ticks {
        let x = slider_position(x0, x1, tick, spec.min, spec.max);
        surf.brush_line(x, y - 10, x, y + 10, 2, FADED);
    }
    surf.stamp(
        slider_position(x0, x1, spec.value, spec.min, spec.max),
        y,
        14,
        0x0000,
    );
}

fn no_op_gesture(tool: PointerTool, at: Point) -> Gesture {
    Gesture::Swipe {
        tool,
        from: at,
        to: at,
        bounds: HitRect::from_xywh(at.x, at.y, 1, 1),
    }
}

#[cfg(test)]
#[path = "settings/tests.rs"]
mod tests;
