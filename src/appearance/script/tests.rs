use super::*;

fn test_font() -> FontBook {
    FontBook::for_test(
        ab_glyph::FontRef::try_from_slice(include_bytes!(
            "../../../fonts/ChenYuluoyan-2.0-Thin.ttf"
        ))
        .unwrap(),
        None,
    )
}

#[test]
fn pipeline_produces_strokes() {
    let font = test_font();
    let mut line = rasterize_line(&font, "Yes, Harry?", 96.0);
    assert!(line.width > 100 && line.height > 50);
    let inked_before: usize = line.mask.iter().filter(|&&v| v).count();
    thin(&mut line);
    let inked_after: usize = line.mask.iter().filter(|&&v| v).count();
    assert!(
        inked_after * 3 < inked_before,
        "thinning should slim the glyphs: {inked_before} -> {inked_after}"
    );
    let strokes = trace(&line);
    assert!(!strokes.is_empty());
    let total: usize = strokes.iter().map(|s| s.len()).sum();
    println!(
        "strokes={} total_points={} ({}x{})",
        strokes.len(),
        total,
        line.width,
        line.height
    );
    assert!(total > 200, "expected a decent path length, got {total}");
    let lines = wrap(
        &font,
        "Do you know anything about the Chamber of Secrets?",
        96.0,
        1380.0,
    );
    assert!(lines.len() >= 2);
}

#[test]
fn wraps_unspaced_chinese_without_orphaning_punctuation() {
    let font = test_font();
    for c in "你好世界回答問題這是一段繁體中文已經說話嗎".chars() {
        assert_ne!(
            font.font(font.selected()).glyph_id(c).0,
            0,
            "font is missing Chinese glyph {c}"
        );
    }

    let max = measure(&font, "你好，", 96.0) + 1.0;
    let lines = wrap(&font, "你好，世界。回答問題！", 96.0, max);
    assert!(lines.len() >= 3, "expected Chinese text to wrap: {lines:?}");
    assert_eq!(lines.concat(), "你好，世界。回答問題！");
    assert!(lines
        .iter()
        .all(|line| !line.starts_with(['，', '。', '！'])));

    let mut rendered = rasterize_line(&font, "你好，世界。", 96.0);
    thin(&mut rendered);
    assert!(
        !trace(&rendered).is_empty(),
        "Chinese glyphs should produce pen strokes"
    );
}

#[test]
fn calibration_changes_measurement_and_rasterization_together() {
    let mut font = test_font();
    let normal_width = measure(&font, "字體大小", 80.0);
    let normal = rasterize_line(&font, "字體大小", 80.0);
    font.set_scale_for_test(FontId::ChenYuluoyan, 150);
    let calibrated_width = measure(&font, "字體大小", 80.0);
    let calibrated = rasterize_line(&font, "字體大小", 80.0);
    assert!(calibrated_width > normal_width * 1.45);
    assert!(calibrated.width > normal.width);
    assert!(calibrated.height > normal.height);
}

#[test]
fn hard_wraps_a_single_token_to_the_requested_width() {
    let font = test_font();
    let max = measure(&font, "abcd", 72.0) + 1.0;
    let lines = wrap(&font, "abcdefghijklmnopqrstuvwxyz", 72.0, max);
    assert!(lines.len() > 1);
    assert!(lines
        .iter()
        .all(|line| measure(&font, line, 72.0) <= max + 1.0));
    assert_eq!(lines.concat(), "abcdefghijklmnopqrstuvwxyz");
}

#[test]
fn calibrated_line_height_tracks_the_resolved_font() {
    let mut font = test_font();
    let before = line_height(&font, "回答", 80.0);
    font.set_scale_for_test(FontId::ChenYuluoyan, 180);
    assert!(line_height(&font, "回答", 80.0) > before * 1.7);
}

#[test]
fn handwriting_calibration_never_changes_ui_metrics() {
    let mut font = test_font();
    let before = measure_ui(&font, "字体与大小", 80.0);
    let before_raster = rasterize_ui_line(&font, "帮助", 80.0);
    font.set_scale_for_test(FontId::ChenYuluoyan, 180);
    assert_eq!(measure_ui(&font, "字体与大小", 80.0), before);
    let after_raster = rasterize_ui_line(&font, "帮助", 80.0);
    assert_eq!(after_raster.width, before_raster.width);
    assert_eq!(after_raster.height, before_raster.height);
}
