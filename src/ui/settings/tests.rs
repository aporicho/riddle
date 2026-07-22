use super::*;
use ab_glyph::FontRef;

fn setup() -> (Vec<u8>, Surface, FontBook, SettingsPanel) {
    crate::fb::test_init_screen();
    let (w, h) = (screen_w(), screen_h());
    let mut bytes = vec![0xff; w * h * 4];
    let mut surface = Surface::new(
        bytes.as_mut_ptr(),
        bytes.len(),
        w,
        h,
        w * 4,
        crate::surface::PixFmt::Rgb32,
    );
    let fonts = FontBook::for_test(
        FontRef::try_from_slice(include_bytes!("../../../fonts/DancingScript.ttf")).unwrap(),
        None,
    );
    let panel = SettingsPanel::show(&mut surface, &fonts, PreferenceValues::default());
    (bytes, surface, fonts, panel)
}

#[test]
fn cleanup_strength_toggles_and_blank_dismisses() {
    let (_bytes, _surface, _fonts, panel) = setup();
    let row = panel
        .rows
        .iter()
        .find(|row| row.setting == Setting::CleanupStrength)
        .unwrap();
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Pen,
            at: Point::new(row.card.x0 + 20, row.card.y0 + 20),
        }),
        Some(Action::SetCleanupStrength(CleanupStrength::Standard))
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
fn sliders_map_to_documented_endpoints() {
    let (_bytes, _surface, _fonts, panel) = setup();
    for (setting, left, right) in [
        (
            Setting::CleanupPadding,
            Action::SetCleanupPadding(0),
            Action::SetCleanupPadding(32),
        ),
        (
            Setting::FullRefreshInterval,
            Action::SetFullRefreshInterval(0),
            Action::SetFullRefreshInterval(10),
        ),
    ] {
        let row = panel
            .rows
            .iter()
            .find(|row| row.setting == setting)
            .unwrap();
        let control = row.slider.unwrap();
        assert_eq!(panel.slider_action(setting, control.x0), left);
        assert_eq!(panel.slider_action(setting, control.x1), right);
    }
    let row = panel
        .rows
        .iter()
        .find(|row| row.setting == Setting::AnswerDwell)
        .unwrap();
    assert_eq!(
        panel.slider_action(Setting::AnswerDwell, row.slider.unwrap().x1),
        Action::SetAnswerDwell(200)
    );
}

#[test]
fn font_and_full_refresh_rows_are_explicit_actions() {
    let (_bytes, _surface, _fonts, panel) = setup();
    for (setting, expected) in [
        (Setting::Fonts, Action::OpenFonts),
        (Setting::RefreshNow, Action::RefreshNow),
    ] {
        let row = panel
            .rows
            .iter()
            .find(|row| row.setting == setting)
            .unwrap();
        assert_eq!(
            panel.interact(Gesture::Tap {
                tool: PointerTool::Finger,
                at: Point::new(row.card.x0 + 20, row.card.y0 + 20),
            }),
            Some(expected)
        );
    }
}

#[test]
fn slider_preview_changes_only_its_draft_until_release() {
    let (_bytes, mut surface, fonts, panel) = setup();
    let row = panel
        .rows
        .iter()
        .find(|row| row.setting == Setting::CleanupPadding)
        .unwrap();
    let control = row.slider.unwrap();
    let mut preview = panel
        .begin_preview(
            PointerTool::Pen,
            Point::new(control.x0 + 24, (control.y0 + control.y1) / 2),
        )
        .unwrap();
    assert!(preview.update(Point::new(control.x1 - 24, (control.y0 + control.y1) / 2)));
    preview.render(&mut surface, &fonts);
    assert_eq!(panel.values.cleanup_padding_px, 16);
}
