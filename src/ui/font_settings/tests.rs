use super::super::pointer::PointerTool;
use super::*;

fn test_font() -> FontBook {
    FontBook::for_test(
        ab_glyph::FontRef::try_from_slice(include_bytes!("../../../fonts/DancingScript.ttf"))
            .unwrap(),
        None,
    )
}

fn panel() -> FontPanel {
    FontPanel {
        rows: vec![
            Row {
                id: FontId::ChenYuluoyan,
                y0: 300,
                geometry: geometry(300, 430),
            },
            Row {
                id: FontId::Farstar851,
                y0: 500,
                geometry: geometry(500, 630),
            },
        ],
    }
}

fn geometry(y: i32, slider_y: i32) -> RowGeometry {
    RowGeometry {
        card: HitRect::from_xywh(100, y, 1300, 190),
        title: HitRect::from_xywh(120, y + 20, 500, 60),
        preview: HitRect::from_xywh(120, y + 90, 700, 60),
        slider_control: HitRect::from_xywh(150, slider_y - 38, 1280, 76),
        slider_x0: 180,
        slider_x1: 1400,
        slider_y,
    }
}

#[test]
fn short_row_tap_selects_and_blank_tap_dismisses() {
    let mut panel = panel();
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Finger,
            at: Point::new(500, 550),
        }),
        Some(Action::Select(FontId::Farstar851))
    );
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Finger,
            at: Point::new(500, 900),
        }),
        Some(Action::Dismiss)
    );
}

#[test]
fn long_stroke_never_selects() {
    let mut panel = panel();
    assert_eq!(
        panel.interact(Gesture::Strike {
            tool: PointerTool::Pen,
            from: Point::new(200, 550),
            to: Point::new(600, 550),
            bounds: HitRect::from_xywh(200, 548, 401, 5),
        }),
        Some(Action::Redraw)
    );
}

#[test]
fn slider_tap_and_drag_map_to_calibration_range() {
    let mut panel = panel();
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Finger,
            at: Point::new(180, 630),
        }),
        Some(Action::SetScale(FontId::Farstar851, MIN_SCALE_PERCENT))
    );
    assert_eq!(
        panel.interact(Gesture::Swipe {
            tool: PointerTool::Finger,
            from: Point::new(600, 630),
            to: Point::new(1400, 630),
            bounds: HitRect::from_xywh(600, 628, 801, 5),
        }),
        Some(Action::SetScale(FontId::Farstar851, MAX_SCALE_PERCENT))
    );
}

#[test]
fn card_press_cancels_on_slider_and_slider_preview_tracks_drag_live() {
    let mut panel = panel();
    let mut card = panel
        .begin_preview(PointerTool::Finger, Point::new(500, 550))
        .unwrap();
    assert!(card.is_visible());
    assert!(card.update(Point::new(600, 630)));
    assert!(!card.is_visible());
    assert_eq!(
        panel.interact(card.release_gesture(Point::new(600, 630))),
        Some(Action::Redraw)
    );

    let mut slider = panel
        .begin_preview(PointerTool::Finger, Point::new(600, 630))
        .unwrap();
    assert!(slider.is_visible());
    assert!(slider.update(Point::new(1400, 630)));
    assert!(matches!(
        slider,
        Preview::Slider {
            percent: MAX_SCALE_PERCENT,
            ..
        }
    ));
    assert_eq!(
        panel.interact(slider.release_gesture(Point::new(1400, 630))),
        Some(Action::SetScale(FontId::Farstar851, MAX_SCALE_PERCENT))
    );
}

#[test]
fn slider_hit_region_is_exactly_the_drawn_control() {
    let panel = panel();
    assert_eq!(
        panel.hit_test(Point::new(150, 630)),
        Hit::Slider(FontId::Farstar851)
    );
    assert_eq!(
        panel.hit_test(Point::new(149, 630)),
        Hit::Card(FontId::Farstar851)
    );
}

#[test]
fn rendered_cards_and_slider_controls_match_hit_regions() {
    crate::fb::test_init_screen();
    let (w, h) = (screen_w(), screen_h());
    let mut pixels = vec![0xFF; w * h * 4];
    let mut surf = Surface::new(
        pixels.as_mut_ptr(),
        pixels.len(),
        w,
        h,
        w * 4,
        crate::surface::PixFmt::Rgb32,
    );
    let fonts = test_font();
    let panel = FontPanel::show(&mut surf, &fonts);
    let geometry = panel.row_geometry(FontId::ChenYuluoyan).unwrap();
    assert!(geometry.title.width() > 0 && geometry.preview.width() > 0);
    assert!(surf.luma(geometry.card.x0, geometry.card.y0) < 200);
    assert!(
        surf.luma(geometry.slider_control.x0, geometry.slider_control.y0) < 200,
        "slider hit control must have a visible border"
    );
    let slider_y = (geometry.slider_control.y0 + geometry.slider_control.y1) / 2;
    assert_eq!(
        panel.hit_test(Point::new(geometry.slider_control.x0, slider_y)),
        Hit::Slider(FontId::ChenYuluoyan)
    );
    assert_eq!(
        panel.hit_test(Point::new(geometry.slider_control.x0 - 1, slider_y)),
        Hit::Card(FontId::ChenYuluoyan)
    );
}
