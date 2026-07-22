use super::*;

fn test_font() -> FontBook {
    FontBook::for_test(
        ab_glyph::FontRef::try_from_slice(include_bytes!("../../../fonts/DancingScript.ttf"))
            .unwrap(),
        None,
    )
}

fn panel_with_rows() -> PaperList {
    PaperList {
        saved: Vec::new(),
        rows: vec![
            Row {
                number: 1,
                card: HitRect::from_xywh(100, 300, 1300, 170),
                text: HitRect::from_xywh(200, 330, 700, 60),
                toggle_box: Some(HitRect::from_xywh(1300, 330, 60, 60)),
            },
            Row {
                number: 2,
                card: HitRect::from_xywh(100, 470, 1300, 170),
                text: HitRect::from_xywh(200, 520, 700, 60),
                toggle_box: Some(HitRect::from_xywh(1300, 520, 60, 60)),
            },
        ],
        selectable: false,
        page: 0,
        page_size: 12,
        total_rows: 2,
    }
}

#[test]
fn horizontal_strike_selects_its_row() {
    let mut panel = panel_with_rows();
    assert_eq!(
        panel.interact(Gesture::Strike {
            tool: PointerTool::Pen,
            from: Point::new(200, 550),
            to: Point::new(900, 553),
            bounds: HitRect::from_xywh(200, 548, 701, 7),
        }),
        Some(Action::Delete(2))
    );
}

#[test]
fn actual_text_tap_stays_open_but_right_hand_blank_dismisses() {
    let mut panel = panel_with_rows();
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Finger,
            at: Point::new(800, 350),
        }),
        Some(Action::Redraw)
    );
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Finger,
            at: Point::new(1100, 350),
        }),
        Some(Action::Dismiss)
    );
}

#[test]
fn status_box_uses_its_exact_visible_rectangle() {
    let mut panel = panel_with_rows();
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Finger,
            at: Point::new(1330, 550),
        }),
        Some(Action::Toggle(2))
    );
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Finger,
            at: Point::new(1299, 550),
        }),
        Some(Action::Dismiss)
    );
}

#[test]
fn finger_strike_and_pen_strike_outside_text_never_delete() {
    let mut panel = panel_with_rows();
    assert_eq!(
        panel.interact(Gesture::Strike {
            tool: PointerTool::Finger,
            from: Point::new(200, 550),
            to: Point::new(900, 550),
            bounds: HitRect::from_xywh(200, 548, 701, 5),
        }),
        Some(Action::Redraw)
    );
    assert_eq!(
        panel.interact(Gesture::Strike {
            tool: PointerTool::Pen,
            from: Point::new(950, 550),
            to: Point::new(1200, 550),
            bounds: HitRect::from_xywh(950, 548, 251, 5),
        }),
        Some(Action::Redraw)
    );
    assert_eq!(
        panel.interact(Gesture::Strike {
            tool: PointerTool::Pen,
            from: Point::new(1000, 550),
            to: Point::new(400, 550),
            bounds: HitRect::from_xywh(400, 548, 601, 5),
        }),
        Some(Action::Redraw),
        "crossing text is not destructive unless the pen began in it"
    );
}

#[test]
fn preview_inverts_controls_but_only_pen_text_can_preview_a_strike() {
    let panel = panel_with_rows();
    let toggle = panel
        .begin_preview(PointerTool::Finger, Point::new(1330, 550))
        .unwrap();
    assert!(toggle.is_visible());
    assert_eq!(toggle.rect(), HitRect::from_xywh(1300, 520, 60, 60));

    assert!(panel
        .begin_preview(PointerTool::Finger, Point::new(300, 550))
        .is_none());
    let mut strike = panel
        .begin_preview(PointerTool::Pen, Point::new(300, 550))
        .unwrap();
    assert!(!strike.is_visible(), "down must not paint a dot");
    assert!(!strike.update(Point::new(390, 552)));
    assert!(!strike.is_visible());
    assert!(strike.update(Point::new(500, 552)));
    assert!(strike.is_visible());
}

#[test]
fn selectable_rows_open_on_tap_and_never_delete_on_strike() {
    let mut panel = panel_with_rows();
    panel.selectable = true;
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: PointerTool::Finger,
            at: Point::new(1100, 550),
        }),
        Some(Action::Select(2))
    );
    assert_eq!(
        panel.interact(Gesture::Strike {
            tool: PointerTool::Pen,
            from: Point::new(200, 550),
            to: Point::new(900, 553),
            bounds: HitRect::from_xywh(200, 548, 701, 7),
        }),
        Some(Action::Redraw)
    );
}

#[test]
fn vertical_strokes_page_without_deleting_rows() {
    let mut panel = panel_with_rows();
    panel.total_rows = 20;
    panel.page_size = 12;
    assert_eq!(
        panel.interact(Gesture::Swipe {
            tool: PointerTool::Finger,
            from: Point::new(500, 1200),
            to: Point::new(498, 300),
            bounds: HitRect::from_xywh(498, 300, 3, 901),
        }),
        Some(Action::Redraw)
    );
    assert_eq!(panel.page, 1);
    assert_eq!(
        panel.interact(Gesture::Swipe {
            tool: PointerTool::Pen,
            from: Point::new(500, 300),
            to: Point::new(498, 1200),
            bounds: HitRect::from_xywh(498, 300, 3, 901),
        }),
        Some(Action::Redraw)
    );
    assert_eq!(panel.page, 0);
}

#[test]
fn rendered_geometry_is_the_hit_geometry() {
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
    let font = test_font();
    let entries = vec!["短任务".to_string()];
    let enabled = [true];
    let panel = PaperList::show(
        &mut surf,
        &font,
        "任务",
        "暂无",
        "点击空白退出",
        &entries,
        Some(&enabled),
    );
    let row = panel.rows[0];
    let toggle = row.toggle_box.unwrap();
    assert!(row.text.width() > 0 && row.text.height() > 0);
    assert_eq!(
        panel.hit_test(Point::new(row.text.x1, (row.text.y0 + row.text.y1) / 2)),
        Hit::Blank,
        "the exclusive edge immediately right of the rasterized text is blank"
    );
    assert_eq!(
        panel.hit_test(Point::new(toggle.x0, toggle.y0)),
        Hit::Toggle(1)
    );
    assert!(surf.luma(toggle.x0, toggle.y0) < 128);

    let reader =
        PaperList::show_selectable(&mut surf, &font, "书库", "暂无", "点击空白退出", &entries);
    let card = reader.rows[0].card;
    assert!(surf.luma(card.x0, card.y0) < 200);
    assert_eq!(
        reader.hit_test(Point::new(card.x1 - 8, (card.y0 + card.y1) / 2)),
        Hit::SelectCard(1),
        "the visible reader card is its complete hit region"
    );
}
