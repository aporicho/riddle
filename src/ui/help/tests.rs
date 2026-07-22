use super::*;

fn stroke(pts: &[(i32, i32)]) -> Vec<(i32, i32, i32)> {
    pts.iter().map(|&(x, y)| (x, y, 3)).collect()
}

/// Parametric "?": hook (arc sweeping over the top and curling back) then
/// a straight descender; optional dot.
fn question_mark(scale: f32, with_dot: bool, reversed: bool) -> Vec<Vec<(i32, i32, i32)>> {
    let mut pts = Vec::new();
    let (cx, cy, r) = (200.0 * scale, 180.0 * scale, 120.0 * scale);
    let mut deg = 180.0f32;
    while deg <= 450.0 {
        let a = deg.to_radians();
        pts.push(((cx + r * a.cos()) as i32, (cy + r * a.sin()) as i32));
        deg += 6.0;
    }
    let (dx, dy) = (cx as i32, (cy + r) as i32);
    for i in 1..=20 {
        pts.push((dx, dy + (i as f32 * 13.0 * scale) as i32));
    }
    if reversed {
        pts.reverse();
    }
    let mut out = vec![stroke(&pts)];
    if with_dot {
        let ddy = dy + (300.0 * scale) as i32 + 60;
        out.push(stroke(&[(dx - 5, ddy), (dx + 5, ddy + 5), (dx, ddy + 8)]));
    }
    out
}

#[test]
fn detects_question_marks() {
    assert!(looks_like_question_mark(&question_mark(1.5, true, false)));
    assert!(looks_like_question_mark(&question_mark(1.5, false, false)));
    assert!(looks_like_question_mark(&question_mark(1.5, true, true)));
    assert!(looks_like_question_mark(&question_mark(3.0, true, false)));
}

#[test]
fn rejects_non_question_marks() {
    // Too small (normal end-of-sentence "?").
    assert!(!looks_like_question_mark(&question_mark(0.5, true, false)));
    // "!" — vertical bar plus dot.
    let bar: Vec<(i32, i32)> = (0..40).map(|i| (200, 60 + i * 12)).collect();
    assert!(!looks_like_question_mark(&[
        stroke(&bar),
        stroke(&[(200, 600), (204, 604)])
    ]));
    // "7" — flat top bar, diagonal descender.
    let mut seven: Vec<(i32, i32)> = (0..20).map(|i| (80 + i * 12, 60)).collect();
    seven.extend((0..40).map(|i| (320 - i * 4, 60 + i * 12)));
    assert!(!looks_like_question_mark(&[stroke(&seven)]));
    // Two long strokes side by side (writing, not a glyph).
    let l1: Vec<(i32, i32)> = (0..40).map(|i| (100, 60 + i * 10)).collect();
    let l2: Vec<(i32, i32)> = (0..40).map(|i| (400, 60 + i * 10)).collect();
    assert!(!looks_like_question_mark(&[stroke(&l1), stroke(&l2)]));
    // Empty / too many strokes.
    assert!(!looks_like_question_mark(&[]));
    let dot = stroke(&[(0, 0), (1, 1)]);
    assert!(!looks_like_question_mark(&[
        dot.clone(),
        dot.clone(),
        dot.clone(),
        dot
    ]));
}

#[test]
fn modal_renders_and_restores() {
    crate::fb::test_init_screen();
    let (w, h) = (screen_w(), screen_h());
    let mut buf = vec![0xFFu8; w * h * 4];
    let ptr = buf.as_mut_ptr();
    let mut surf = Surface::new(ptr, buf.len(), w, h, w * 4, crate::surface::PixFmt::Rgb32);
    let font = FontBook::for_test(
        ab_glyph::FontRef::try_from_slice(include_bytes!("../../../fonts/DancingScript.ttf"))
            .unwrap(),
        None,
    );

    // Scribble something under the panel area so restore is observable.
    surf.fill_rect(700, 1000, 200, 200, BLACK);
    let before = surf.copy_rect(0, 0, w, h);

    let panel = show(&mut surf, &font, true);
    let (px, py, pw, ph) = panel.region.rect();
    assert!(pw > 400 && ph > 400, "panel too small: {pw}x{ph}");
    let close = panel.close_rect();
    let close_point = Point::new((close.x0 + close.x1) / 2, (close.y0 + close.y1) / 2);
    assert_eq!(panel.hit_test(close_point), HelpHit::Close);
    let mut preview = panel
        .begin_preview(super::super::pointer::PointerTool::Finger, close_point)
        .unwrap();
    assert!(preview.is_visible());
    assert!(preview.update(ordinary_point_outside(preview.rect())));
    assert!(!preview.is_visible());
    assert_eq!(
        panel.interact(preview.release_gesture(ordinary_point_outside(preview.rect()))),
        Some(HelpAction::Consume)
    );
    assert_eq!(
        panel.interact(Gesture::Tap {
            tool: super::super::pointer::PointerTool::Finger,
            at: close_point,
        }),
        Some(HelpAction::Close)
    );
    let ordinary_panel_point = Point::new(panel.panel_rect().x0 + 24, panel.panel_rect().y0 + 24);
    assert_eq!(panel.hit_test(ordinary_panel_point), HelpHit::Panel);
    assert_eq!(panel.hit_test(Point::new(0, 0)), HelpHit::Outside);
    assert_eq!(
        panel.interact(Gesture::Swipe {
            tool: super::super::pointer::PointerTool::Finger,
            from: ordinary_panel_point,
            to: Point::new(ordinary_panel_point.x + 200, ordinary_panel_point.y),
            bounds: HitRect::from_xywh(ordinary_panel_point.x, ordinary_panel_point.y, 201, 1,),
        }),
        Some(HelpAction::Consume),
        "modal gestures must never fall through to page ink"
    );
    // Panel must contain ink (text + frame).
    let mut black = 0;
    for y in py..py + ph {
        for x in px..px + pw {
            if surf.luma(x, y) < 128 {
                black += 1;
            }
        }
    }
    assert!(black > 5000, "panel looks empty: {black} dark px");

    // Dump for visual inspection.
    let out = std::env::temp_dir().join("magicpaper-help-modal.png");
    let mut gray = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            gray[y * w + x] = surf.luma(x as i32, y as i32);
        }
    }
    let file = std::fs::File::create(&out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&gray).unwrap();
    eprintln!("modal snapshot: {}", out.display());

    // Dismissing must restore the page byte-for-byte.
    panel.dismiss(&mut surf);
    assert_eq!(before, surf.copy_rect(0, 0, w, h), "restore is not exact");
}

fn ordinary_point_outside(rect: HitRect) -> Point {
    Point::new(rect.x0.saturating_sub(2), rect.y0.saturating_sub(2))
}

#[test]
fn sleep_page_renders_and_restores() {
    crate::fb::test_init_screen();
    let (w, h) = (screen_w(), screen_h());
    let mut buf = vec![0xFFu8; w * h * 4];
    let ptr = buf.as_mut_ptr();
    let mut surf = Surface::new(ptr, buf.len(), w, h, w * 4, crate::surface::PixFmt::Rgb32);
    let font = FontBook::for_test(
        ab_glyph::FontRef::try_from_slice(include_bytes!("../../../fonts/DancingScript.ttf"))
            .unwrap(),
        None,
    );

    surf.fill_rect(300, 300, 400, 400, BLACK);
    let before = surf.copy_rect(0, 0, w, h);

    let saved = show_sleep(&mut surf, &font);
    let mut black = 0usize;
    for y in 0..h {
        for x in 0..w {
            if surf.luma(x as i32, y as i32) < 128 {
                black += 1;
            }
        }
    }
    assert!(black > 10_000, "sleep page looks empty: {black} dark px");

    let out = std::env::temp_dir().join("magicpaper-sleep-page.png");
    let mut gray = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            gray[y * w + x] = surf.luma(x as i32, y as i32);
        }
    }
    let file = std::fs::File::create(&out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
    enc.set_color(png::ColorType::Grayscale);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&gray).unwrap();
    eprintln!("sleep snapshot: {}", out.display());

    restore_sleep(&mut surf, &saved);
    assert_eq!(
        before,
        surf.copy_rect(0, 0, w, h),
        "sleep restore is not exact"
    );
}

#[test]
fn handwriting_calibration_does_not_resize_the_ui_manual() {
    let face =
        ab_glyph::FontRef::try_from_slice(include_bytes!("../../../fonts/DancingScript.ttf"))
            .unwrap();
    let mut font = FontBook::for_test(face, None);
    font.set_scale_for_test(crate::fonts::FontId::ChenYuluoyan, 180);
    let (title, body, footer) = fitted_base_sizes(BODY_TAKEOVER.len(), 1696);
    let text_height = title * 1.4 + body * 1.3 * (BODY_TAKEOVER.len() as f32 + 0.5) + footer * 1.4;
    assert!(text_height + (2 * PAD) as f32 <= 1656.5);
    assert_eq!(font.calibrated_px(crate::fonts::FontId::Ui, body), body);
}
