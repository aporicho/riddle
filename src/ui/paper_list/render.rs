use crate::fb::screen_w;
use crate::fonts::FontBook;
use crate::script;
use crate::surface::{Surface, BLACK};

use super::HitRect;

pub(super) fn draw_status_box(surf: &mut Surface, x: usize, y: usize, size: usize, active: bool) {
    let thickness = 3;
    surf.fill_rect(x, y, size, thickness, BLACK);
    surf.fill_rect(x, y + size - thickness, size, thickness, BLACK);
    surf.fill_rect(x, y, thickness, size, BLACK);
    surf.fill_rect(x + size - thickness, y, thickness, size, BLACK);
    if active {
        surf.brush_line(
            (x + size / 5) as i32,
            (y + size / 2) as i32,
            (x + size * 2 / 5) as i32,
            (y + size * 4 / 5) as i32,
            3,
            BLACK,
        );
        surf.brush_line(
            (x + size * 2 / 5) as i32,
            (y + size * 4 / 5) as i32,
            (x + size * 4 / 5) as i32,
            (y + size / 5) as i32,
            3,
            BLACK,
        );
    } else {
        surf.brush_line(
            (x + size / 4) as i32,
            (y + size / 4) as i32,
            (x + size * 3 / 4) as i32,
            (y + size * 3 / 4) as i32,
            3,
            BLACK,
        );
        surf.brush_line(
            (x + size * 3 / 4) as i32,
            (y + size / 4) as i32,
            (x + size / 4) as i32,
            (y + size * 3 / 4) as i32,
            3,
            BLACK,
        );
    }
}

pub(in crate::ui) fn fit_line(font: &FontBook, text: &str, size: f32, max_width: usize) -> String {
    if script::measure_ui(font, text, size) as usize <= max_width {
        return text.to_string();
    }
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate = format!("{}…", chars.iter().collect::<String>());
        if script::measure_ui(font, &candidate, size) as usize <= max_width {
            return candidate;
        }
    }
    "…".into()
}

pub(in crate::ui) fn blit_left(
    surf: &mut Surface,
    font: &FontBook,
    text: &str,
    size: f32,
    x: usize,
    y: usize,
) -> HitRect {
    let line = script::rasterize_ui_line(font, text, size);
    for row in 0..line.height {
        for col in 0..line.width {
            if line.mask[row * line.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
    HitRect::from_xywh(x as i32, y as i32, line.width as i32, line.height as i32)
}

pub(in crate::ui) fn draw_frame(surf: &mut Surface, rect: HitRect, thickness: usize, color: u16) {
    let x = rect.x0.max(0) as usize;
    let y = rect.y0.max(0) as usize;
    let width = rect.width().max(0) as usize;
    let height = rect.height().max(0) as usize;
    if width < thickness * 2 || height < thickness * 2 {
        return;
    }
    surf.fill_rect(x, y, width, thickness, color);
    surf.fill_rect(x, y + height - thickness, width, thickness, color);
    surf.fill_rect(x, y, thickness, height, color);
    surf.fill_rect(x + width - thickness, y, thickness, height, color);
}

pub(in crate::ui) fn blit_centered(
    surf: &mut Surface,
    font: &FontBook,
    text: &str,
    size: f32,
    y: usize,
) {
    let line = script::rasterize_ui_line(font, text, size);
    let x = screen_w().saturating_sub(line.width) / 2;
    for row in 0..line.height {
        for col in 0..line.width {
            if line.mask[row * line.width + col] {
                surf.put_px((x + col) as i32, (y + row) as i32, BLACK);
            }
        }
    }
}
