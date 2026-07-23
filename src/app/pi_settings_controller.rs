//! Persistence and paper transitions for Pi settings.
//!
//! Runtime transport deliberately stays outside this module. Operational cards
//! return a typed [`PiAgentAction`] for the ReMagic/Pi integration boundary.

use crate::display::Display;
use crate::fonts::FontBook;
use crate::pi_preferences::{PiPreferenceValues, PiPreferences};
use crate::platform::RefreshIntent;
use crate::surface::Surface;
use crate::ui;
use crate::ui::pi_settings::PiAgentStatus;
use crate::ui::pointer::Gesture;

use super::state::State;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PiAgentAction {
    ReloadProfile,
    TestConnection,
    RestartAgent,
    NewSession,
}

pub(super) fn open(state: &mut State, surface: &mut Surface, fonts: &FontBook, display: &Display) {
    let old = std::mem::replace(state, State::Listening { last_pen: None });
    let State::Settings { panel: settings } = old else {
        *state = old;
        return;
    };
    let panel = ui::pi_settings::PiSettingsPanel::show(
        surface,
        fonts,
        PiPreferences::open().values(),
        PiAgentStatus::Waiting,
    );
    *state = State::PiSettings { panel, settings };
    display.present_all(surface.w, surface.h, RefreshIntent::Content);
    eprintln!("magic-paper: Pi settings opened from settings");
}

pub(super) fn finish_stroke(
    state: &mut State,
    surface: &mut Surface,
    fonts: &FontBook,
    display: &Display,
    gesture: Gesture,
) -> Option<PiAgentAction> {
    let action = match state {
        State::PiSettings { panel, .. } => panel.interact(gesture),
        _ => return None,
    };
    match action {
        Some(ui::pi_settings::Action::SetProvider(provider)) => {
            update(state, surface, fonts, display, |values| {
                values.provider = provider;
                values.thinking = values.thinking.normalized(provider);
            });
            return Some(PiAgentAction::ReloadProfile);
        }
        Some(ui::pi_settings::Action::ToggleModel) => {
            update(state, surface, fonts, display, |values| {
                values.model = values.model.toggled();
            });
            return Some(PiAgentAction::ReloadProfile);
        }
        Some(ui::pi_settings::Action::SetThinking(thinking)) => {
            update(state, surface, fonts, display, |values| {
                values.thinking = thinking;
            });
            return Some(PiAgentAction::ReloadProfile);
        }
        Some(ui::pi_settings::Action::SetTools(enabled)) => {
            update(state, surface, fonts, display, |values| {
                values.tools_enabled = enabled;
            });
            return Some(PiAgentAction::ReloadProfile);
        }
        Some(ui::pi_settings::Action::TestConnection) => {
            return Some(PiAgentAction::TestConnection);
        }
        Some(ui::pi_settings::Action::RestartAgent) => {
            return Some(PiAgentAction::RestartAgent);
        }
        Some(ui::pi_settings::Action::NewSession) => {
            return Some(PiAgentAction::NewSession);
        }
        Some(ui::pi_settings::Action::Dismiss) => dismiss(state, surface, display),
        Some(ui::pi_settings::Action::Redraw) => redraw(
            state,
            surface,
            fonts,
            display,
            PiPreferences::open().values(),
            PiAgentStatus::Waiting,
        ),
        None => {}
    }
    None
}

pub(super) fn set_status(
    state: &mut State,
    surface: &mut Surface,
    fonts: &FontBook,
    display: &Display,
    status: crate::oracle::AgentControlStatus,
) {
    let State::PiSettings { panel, .. } = state else {
        return;
    };
    let status = match status {
        crate::oracle::AgentControlStatus::Starting => PiAgentStatus::Starting,
        crate::oracle::AgentControlStatus::Online => PiAgentStatus::Online,
        crate::oracle::AgentControlStatus::MissingKey => PiAgentStatus::MissingKey,
        crate::oracle::AgentControlStatus::MissingRuntime => PiAgentStatus::MissingRuntime,
        crate::oracle::AgentControlStatus::StorageError => PiAgentStatus::StorageError,
        crate::oracle::AgentControlStatus::NetworkError => PiAgentStatus::NetworkError,
    };
    let region = panel.set_status(surface, fonts, status);
    display.present_region(
        region.x0,
        region.y0,
        region.width(),
        region.height(),
        RefreshIntent::Ui,
    );
}

fn update(
    state: &mut State,
    surface: &mut Surface,
    fonts: &FontBook,
    display: &Display,
    update: impl FnOnce(&mut PiPreferenceValues),
) {
    let mut preferences = PiPreferences::open();
    let mut values = preferences.values();
    update(&mut values);
    if let Err(error) = preferences.replace(values) {
        eprintln!("magic-paper: could not persist Pi settings: {error}");
    }
    redraw(
        state,
        surface,
        fonts,
        display,
        preferences.values(),
        PiAgentStatus::Waiting,
    );
}

fn redraw(
    state: &mut State,
    surface: &mut Surface,
    fonts: &FontBook,
    display: &Display,
    values: PiPreferenceValues,
    status: PiAgentStatus,
) {
    if let State::PiSettings { panel, .. } = state {
        panel.redraw(surface, fonts, values, status);
    }
    display.present_all(surface.w, surface.h, RefreshIntent::Content);
}

fn dismiss(state: &mut State, surface: &mut Surface, display: &Display) {
    let old = std::mem::replace(state, State::Listening { last_pen: None });
    if let State::PiSettings { panel, settings } = old {
        panel.dismiss(surface);
        *state = State::Settings { panel: settings };
    } else {
        *state = old;
    }
    display.present_all(surface.w, surface.h, RefreshIntent::Content);
    eprintln!("magic-paper: Pi settings returned to settings");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operational_actions_are_transport_agnostic_and_stable() {
        assert_eq!(
            format!("{:?}", PiAgentAction::TestConnection),
            "TestConnection"
        );
        assert_ne!(PiAgentAction::RestartAgent, PiAgentAction::NewSession);
        assert_ne!(PiAgentAction::ReloadProfile, PiAgentAction::NewSession);
    }
}
