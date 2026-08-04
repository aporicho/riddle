//! Application actions for the paper-native experience settings panel.

use crate::display::Display;
use crate::fonts::FontBook;
use crate::platform::RefreshIntent;
use crate::preferences::PreferenceValues;
use crate::surface::Surface;
use crate::ui;
use crate::ui::pointer::Gesture;

use super::layers::ModalLayers;
use super::pi_settings_controller;
use super::refresh_controller::RefreshController;
use super::state::{FontOrigin, State};

pub(super) fn finish_settings_stroke(
    state: &mut State,
    modal_layers: &mut ModalLayers,
    surface: &mut Surface,
    fonts: &mut FontBook,
    display: &Display,
    refresh: &mut RefreshController,
    gesture: Gesture,
) {
    let action = match state {
        State::Settings { panel } => panel.interact(gesture),
        _ => return,
    };
    match action {
        Some(ui::settings::Action::SetCleanupStrength(value)) => {
            update_values(
                state,
                modal_layers,
                surface,
                fonts,
                display,
                refresh,
                |values| {
                    values.cleanup_strength = value;
                },
            );
        }
        Some(ui::settings::Action::SetCleanupPadding(value)) => {
            update_values(
                state,
                modal_layers,
                surface,
                fonts,
                display,
                refresh,
                |values| {
                    values.cleanup_padding_px = value;
                },
            );
        }
        Some(ui::settings::Action::SetFullRefreshInterval(value)) => {
            update_values(
                state,
                modal_layers,
                surface,
                fonts,
                display,
                refresh,
                |values| {
                    values.full_refresh_every_replies = value;
                },
            );
        }
        Some(ui::settings::Action::SetAnswerDwell(value)) => {
            update_values(
                state,
                modal_layers,
                surface,
                fonts,
                display,
                refresh,
                |values| {
                    values.answer_dwell_percent = value;
                },
            );
        }
        Some(ui::settings::Action::OpenPiAgent) => {
            pi_settings_controller::open(state, modal_layers, surface, fonts, display)
        }
        Some(ui::settings::Action::OpenFonts) => {
            open_fonts(state, modal_layers, surface, fonts, display)
        }
        Some(ui::settings::Action::RefreshNow) => {
            refresh.request_full(display, surface.w, surface.h);
            eprintln!("magic-paper: settings requested a full refresh");
        }
        Some(ui::settings::Action::Dismiss) => {
            let old = std::mem::replace(state, State::Listening { last_pen: None });
            let _ = old;
            modal_layers.dismiss(surface);
            display.present_all(surface.w, surface.h, RefreshIntent::Content);
            eprintln!("magic-paper: settings dismissed");
        }
        Some(ui::settings::Action::Redraw) => {
            redraw(
                state,
                modal_layers,
                surface,
                fonts,
                display,
                refresh.values(),
            );
        }
        None => {}
    }
}

fn update_values(
    state: &mut State,
    modal_layers: &mut ModalLayers,
    surface: &mut Surface,
    fonts: &FontBook,
    display: &Display,
    refresh: &mut RefreshController,
    update: impl FnOnce(&mut PreferenceValues),
) {
    let mut values = refresh.values();
    update(&mut values);
    if let Err(error) = refresh.replace_values(values) {
        eprintln!("magic-paper: could not persist settings: {error}");
    }
    redraw(
        state,
        modal_layers,
        surface,
        fonts,
        display,
        refresh.values(),
    );
}

fn redraw(
    state: &mut State,
    modal_layers: &mut ModalLayers,
    surface: &mut Surface,
    fonts: &FontBook,
    display: &Display,
    values: PreferenceValues,
) {
    if let State::Settings { panel } = state {
        let _ = modal_layers.with_ui(|ui| panel.redraw(ui, fonts, values));
    }
    modal_layers.compose_all(surface);
    display.present_all(surface.w, surface.h, RefreshIntent::Content);
}

fn open_fonts(
    state: &mut State,
    modal_layers: &mut ModalLayers,
    surface: &mut Surface,
    fonts: &FontBook,
    display: &Display,
) {
    let old = std::mem::replace(state, State::Listening { last_pen: None });
    let State::Settings { panel: settings } = old else {
        *state = old;
        return;
    };
    let panel =
        modal_layers.replace_ui(surface, |ui| ui::font_settings::FontPanel::show(ui, fonts));
    *state = State::FontList {
        panel,
        origin: FontOrigin::Settings(settings),
    };
    modal_layers.compose_all(surface);
    display.present_all(surface.w, surface.h, RefreshIntent::Content);
    eprintln!("magic-paper: font settings opened from settings");
}
