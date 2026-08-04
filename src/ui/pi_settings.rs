//! Paper-native Pi agent preferences and command surface.

use crate::fb::{screen_h, screen_w};
use crate::fonts::FontBook;
use crate::pi_preferences::{PiPreferenceValues, PiProvider, PiThinking};
use crate::surface::{Surface, FADED, WHITE};

use super::paper_list::render::{blit_centered, blit_left, draw_frame, fit_line};
use super::pointer::{invert_mono, Gesture, HitRect, Point, PointerTool};

const SIDE: usize = 72;
const HEADER_Y: usize = 78;
const STATUS_TOP: usize = 214;
const STATUS_H: usize = 150;
const GRID_TOP: usize = 392;
const GRID_ROW_H: usize = 190;
const GRID_GAP: usize = 24;
const ACTION_TOP: usize = 820;
const ACTION_H: usize = 170;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[allow(dead_code)] // Runtime transport constructs the non-waiting snapshots.
pub(crate) enum PiAgentStatus {
    #[default]
    Waiting,
    Starting,
    Online,
    MissingKey,
    MissingRuntime,
    StorageError,
    NetworkError,
}

impl PiAgentStatus {
    const fn label(self) -> &'static str {
        match self {
            Self::Waiting => "等待 ReMagic Agent 连接",
            Self::Starting => "Agent 正在启动",
            Self::Online => "Pi 配置已就绪",
            Self::MissingKey => "缺少供应商密钥",
            Self::MissingRuntime => "缺少 ReMagic Pi 运行时",
            Self::StorageError => "无法保存本地会话边界",
            Self::NetworkError => "无法连接 ReMagic Agent",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Setting {
    Provider,
    Model,
    Thinking,
    Tools,
    TestConnection,
    RestartAgent,
    NewSession,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    SetProvider(PiProvider),
    ToggleModel,
    SetThinking(PiThinking),
    SetTools(bool),
    TestConnection,
    RestartAgent,
    NewSession,
    Dismiss,
    Redraw,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Card(Setting),
    Inert,
    Blank,
}

#[derive(Clone, Copy, Debug)]
struct Card {
    setting: Setting,
    rect: HitRect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Preview {
    tool: PointerTool,
    setting: Setting,
    card: HitRect,
    start: Point,
    pressed: bool,
}

impl Preview {
    pub(crate) const fn rect(self) -> HitRect {
        self.card
    }

    pub(crate) const fn is_visible(self) -> bool {
        self.pressed
    }

    pub(crate) fn update(&mut self, point: Point) -> bool {
        let next = self.card.contains(point);
        let changed = self.pressed != next;
        self.pressed = next;
        changed
    }

    pub(crate) fn render(self, surface: &mut Surface) {
        if self.pressed {
            invert_mono(surface, self.card);
        }
    }

    pub(crate) fn release_gesture(self, end: Point) -> Gesture {
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

pub(crate) struct PiSettingsPanel {
    cards: Vec<Card>,
    status_rect: HitRect,
    values: PiPreferenceValues,
    status: PiAgentStatus,
}

impl PiSettingsPanel {
    pub(crate) fn show(
        surface: &mut Surface,
        fonts: &FontBook,
        values: PiPreferenceValues,
        status: PiAgentStatus,
    ) -> Self {
        let mut panel = Self {
            cards: Vec::new(),
            status_rect: HitRect::from_xywh(0, 0, 1, 1),
            values,
            status,
        };
        panel.redraw(surface, fonts, values, status);
        panel
    }

    pub(crate) fn redraw(
        &mut self,
        surface: &mut Surface,
        fonts: &FontBook,
        values: PiPreferenceValues,
        status: PiAgentStatus,
    ) {
        self.values = values;
        self.status = status;
        self.cards.clear();
        surface.fill_rect(0, 0, screen_w(), screen_h(), WHITE);
        blit_centered(surface, fonts, "Pi 智能体", 82.0, HEADER_Y);
        self.status_rect = full_card(STATUS_TOP, STATUS_H);
        self.draw_status(surface, fonts);

        let settings = [
            Setting::Provider,
            Setting::Model,
            Setting::Thinking,
            Setting::Tools,
        ];
        for (index, setting) in settings.into_iter().enumerate() {
            let rect = grid_card(index);
            draw_card(surface, fonts, rect, setting_title(setting, values), 46.0);
            self.cards.push(Card { setting, rect });
        }

        for (index, setting) in [
            Setting::TestConnection,
            Setting::RestartAgent,
            Setting::NewSession,
        ]
        .into_iter()
        .enumerate()
        {
            let rect = action_card(index);
            draw_card(surface, fonts, rect, setting_title(setting, values), 42.0);
            self.cards.push(Card { setting, rect });
        }

        draw_footer(surface, fonts);
    }

    pub(crate) fn set_status(
        &mut self,
        surface: &mut Surface,
        fonts: &FontBook,
        status: PiAgentStatus,
    ) -> HitRect {
        self.status = status;
        self.draw_status(surface, fonts);
        self.status_rect
    }

    fn draw_status(&self, surface: &mut Surface, fonts: &FontBook) {
        let rect = self.status_rect;
        surface.fill_rect(
            rect.x0.max(0) as usize,
            rect.y0.max(0) as usize,
            rect.width().max(0) as usize,
            rect.height().max(0) as usize,
            WHITE,
        );
        draw_frame(surface, rect, 2, FADED);
        blit_left(
            surface,
            fonts,
            &format!("状态 · {}", self.status.label()),
            46.0,
            SIDE + 28,
            STATUS_TOP + 43,
        );
    }

    fn hit_test(&self, point: Point) -> Hit {
        if self.status_rect.contains(point) {
            return Hit::Inert;
        }
        self.cards
            .iter()
            .find(|card| card.rect.contains(point))
            .map_or(Hit::Blank, |card| Hit::Card(card.setting))
    }

    pub(crate) fn begin_preview(&self, tool: PointerTool, point: Point) -> Option<Preview> {
        let Hit::Card(setting) = self.hit_test(point) else {
            return None;
        };
        let card = self.cards.iter().find(|card| card.setting == setting)?.rect;
        Some(Preview {
            tool,
            setting,
            card,
            start: point,
            pressed: true,
        })
    }

    pub(crate) fn interact(&self, gesture: Gesture) -> Option<Action> {
        match gesture {
            Gesture::Tap { at, .. } => Some(match self.hit_test(at) {
                Hit::Card(Setting::Provider) => Action::SetProvider(self.values.provider.next()),
                Hit::Card(Setting::Model) => Action::ToggleModel,
                Hit::Card(Setting::Thinking) => {
                    Action::SetThinking(self.values.thinking.next(self.values.provider))
                }
                Hit::Card(Setting::Tools) => Action::SetTools(!self.values.tools_enabled),
                Hit::Card(Setting::TestConnection) => Action::TestConnection,
                Hit::Card(Setting::RestartAgent) => Action::RestartAgent,
                Hit::Card(Setting::NewSession) => Action::NewSession,
                Hit::Inert => Action::Redraw,
                Hit::Blank => Action::Dismiss,
            }),
            Gesture::Swipe { .. } | Gesture::Strike { .. } => Some(Action::Redraw),
        }
    }
}

fn draw_footer(surface: &mut Surface, fonts: &FontBook) {
    blit_centered(
        surface,
        fonts,
        "密钥由 ReMagic 在电脑端安全配置",
        38.0,
        ACTION_TOP + ACTION_H + 54,
    );
    blit_centered(
        surface,
        fonts,
        "点选卡片调整 · 点击空白返回设置",
        38.0,
        screen_h().saturating_sub(94),
    );
}

fn full_card(y: usize, height: usize) -> HitRect {
    HitRect::from_xywh(
        SIDE as i32,
        y as i32,
        screen_w().saturating_sub(SIDE * 2) as i32,
        height as i32,
    )
}

fn grid_card(index: usize) -> HitRect {
    let width = screen_w().saturating_sub(SIDE * 2 + GRID_GAP) / 2;
    let column = index % 2;
    let row = index / 2;
    HitRect::from_xywh(
        (SIDE + column * (width + GRID_GAP)) as i32,
        (GRID_TOP + row * (GRID_ROW_H + GRID_GAP)) as i32,
        width as i32,
        GRID_ROW_H as i32,
    )
}

fn action_card(index: usize) -> HitRect {
    let width = screen_w().saturating_sub(SIDE * 2 + GRID_GAP * 2) / 3;
    HitRect::from_xywh(
        (SIDE + index * (width + GRID_GAP)) as i32,
        ACTION_TOP as i32,
        width as i32,
        ACTION_H as i32,
    )
}

fn setting_title(setting: Setting, values: PiPreferenceValues) -> String {
    match setting {
        Setting::Provider => format!("供应商\n{}", values.provider.label()),
        Setting::Model => format!("模型\n{}", values.model.label()),
        Setting::Thinking => format!("思考\n{}", values.thinking.label()),
        Setting::Tools => format!(
            "Agent 工具\n{}",
            if values.tools_enabled {
                "开启"
            } else {
                "暂停"
            }
        ),
        Setting::TestConnection => "检查配置".into(),
        Setting::RestartAgent => "重启 Pi".into(),
        Setting::NewSession => "新建会话".into(),
    }
}

fn draw_card(surface: &mut Surface, fonts: &FontBook, rect: HitRect, title: String, size: f32) {
    draw_frame(surface, rect, 2, FADED);
    let lines: Vec<&str> = title.split('\n').collect();
    let line_h = (size * 1.35) as usize;
    let block_h = lines.len() * line_h;
    let mut y =
        rect.y0.max(0) as usize + (rect.height().max(0) as usize).saturating_sub(block_h) / 2;
    for line in lines {
        let fitted = fit_line(fonts, line, size, rect.width().max(0) as usize - 28);
        let width = crate::script::measure_ui(fonts, &fitted, size) as usize;
        let x = rect.x0.max(0) as usize + (rect.width().max(0) as usize).saturating_sub(width) / 2;
        blit_left(surface, fonts, &fitted, size, x, y);
        y += line_h;
    }
}

#[cfg(test)]
#[path = "pi_settings/tests.rs"]
mod tests;
