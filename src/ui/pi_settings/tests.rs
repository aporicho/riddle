use super::*;
use ab_glyph::FontRef;

fn setup() -> (Vec<u8>, Surface, FontBook, PiSettingsPanel) {
    crate::fb::test_init_screen();
    let (width, height) = (screen_w(), screen_h());
    let mut bytes = vec![0xff; width * height * 4];
    let mut surface = Surface::new(
        bytes.as_mut_ptr(),
        bytes.len(),
        width,
        height,
        width * 4,
        crate::surface::PixFmt::Rgb32,
    );
    let fonts = FontBook::for_test(
        FontRef::try_from_slice(include_bytes!("../../../fonts/DancingScript.ttf")).unwrap(),
        None,
    );
    let panel = PiSettingsPanel::show(
        &mut surface,
        &fonts,
        PiPreferenceValues::default(),
        PiAgentStatus::Waiting,
    );
    (bytes, surface, fonts, panel)
}

fn tap(panel: &PiSettingsPanel, setting: Setting) -> Option<Action> {
    let card = panel
        .cards
        .iter()
        .find(|card| card.setting == setting)
        .unwrap();
    panel.interact(Gesture::Tap {
        tool: PointerTool::Finger,
        at: Point::new(card.rect.x0 + 20, card.rect.y0 + 20),
    })
}

#[test]
fn preference_cards_cycle_all_paper_visible_options() {
    let (_bytes, _surface, _fonts, panel) = setup();
    assert_eq!(
        tap(&panel, Setting::Provider),
        Some(Action::SetProvider(PiProvider::DeepSeek))
    );
    assert_eq!(tap(&panel, Setting::Model), Some(Action::ToggleModel));
    assert_eq!(
        tap(&panel, Setting::Thinking),
        Some(Action::SetThinking(PiThinking::ExtraHigh))
    );
    assert_eq!(tap(&panel, Setting::Tools), Some(Action::SetTools(false)));
}

#[test]
fn operational_cards_have_explicit_agent_actions() {
    let (_bytes, _surface, _fonts, panel) = setup();
    for (setting, action) in [
        (Setting::TestConnection, Action::TestConnection),
        (Setting::RestartAgent, Action::RestartAgent),
        (Setting::NewSession, Action::NewSession),
    ] {
        assert_eq!(tap(&panel, setting), Some(action));
    }
}

#[test]
fn status_is_inert_and_blank_paper_returns_to_parent_settings() {
    let (_bytes, _surface, _fonts, panel) = setup();
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Pen,
            at: Point::new(panel.status_rect.x0 + 20, panel.status_rect.y0 + 20),
        }),
        Some(Action::Redraw)
    );
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Pen,
            at: Point::new(10, 10),
        }),
        Some(Action::Dismiss)
    );
}

#[test]
fn every_action_card_has_immediate_inverted_preview() {
    let (_bytes, mut surface, _fonts, panel) = setup();
    for card in &panel.cards {
        let point = Point::new(card.rect.x0 + 12, card.rect.y0 + 12);
        let preview = panel
            .begin_preview(PointerTool::Finger, point)
            .expect("interactive cards need feedback");
        assert!(preview.is_visible());
        let before = surface.copy_rect(
            card.rect.x0 as usize,
            card.rect.y0 as usize,
            card.rect.width() as usize,
            card.rect.height() as usize,
        );
        preview.render(&mut surface);
        let after = surface.copy_rect(
            card.rect.x0 as usize,
            card.rect.y0 as usize,
            card.rect.width() as usize,
            card.rect.height() as usize,
        );
        assert_ne!(before, after);
    }
}

#[test]
fn all_cards_fit_on_the_move_screen() {
    let (_bytes, _surface, _fonts, panel) = setup();
    assert!(panel
        .cards
        .iter()
        .all(|card| card.rect.x0 >= 0 && card.rect.x1 <= screen_w() as i32));
    assert!(panel
        .cards
        .iter()
        .all(|card| card.rect.y0 >= 0 && card.rect.y1 < screen_h() as i32));
}
