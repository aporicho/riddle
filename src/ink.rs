//! User ink: capture pen strokes, render them, dissolve them, rasterize them
//! for the oracle.

use crate::fb::BBox;
use crate::platform::RefreshIntent;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(test)]
use crate::surface::BLACK;
use crate::surface::{PixFmt, Surface, WHITE};

mod smoothing;

/// Immutable raw page snapshot. Copying the crop is cheap and bounded; luma
/// conversion, downsampling, and PNG compression can safely run on a worker
/// after the live shared framebuffer starts changing again.
pub struct PageCapture {
    pixels: Vec<u8>,
    width: usize,
    height: usize,
    format: PixFmt,
    max_edge: usize,
}

impl PageCapture {
    pub fn encode_png(&self, cancelled: &AtomicBool) -> std::io::Result<Vec<u8>> {
        let factor = self.width.max(self.height).div_ceil(self.max_edge).max(1);
        let width = (self.width / factor).max(1);
        let height = (self.height / factor).max(1);
        let mut gray = vec![0_u8; width * height];
        for output_y in 0..height {
            if output_y % 16 == 0 && cancelled.load(Ordering::Acquire) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "page encoding cancelled",
                ));
            }
            for output_x in 0..width {
                let mut sum = 0_u32;
                for sample_y in 0..factor {
                    for sample_x in 0..factor {
                        sum += self.luma(output_x * factor + sample_x, output_y * factor + sample_y)
                            as u32;
                    }
                }
                gray[output_y * width + output_x] = (sum / (factor * factor) as u32) as u8;
            }
        }
        if cancelled.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "page encoding cancelled",
            ));
        }
        encode_gray_png(&gray, width, height)
    }

    fn luma(&self, x: usize, y: usize) -> u8 {
        match self.format {
            PixFmt::Rgb565 => {
                let index = (y * self.width + x) * 2;
                let pixel = self.pixels[index] as u16 | (self.pixels[index + 1] as u16) << 8;
                (((pixel >> 5) & 0x3f) as u32 * 255 / 63) as u8
            }
            PixFmt::Rgb32 => self.pixels[(y * self.width + x) * 4 + 1],
        }
    }
}

pub struct Ink {
    /// Finished strokes as point lists (x, y, radius).
    strokes: Vec<Vec<(i32, i32, i32)>>,
    current: Vec<(i32, i32, i32)>,
    last_erase: Option<(i32, i32)>,
    pub bbox: BBox,
}

impl Ink {
    pub fn new() -> Self {
        Self {
            strokes: Vec::new(),
            current: Vec::new(),
            last_erase: None,
            bbox: BBox::empty(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty() && self.current.is_empty()
    }

    /// Finished strokes (the current in-flight stroke is not included).
    pub fn stroke_list(&self) -> &[Vec<(i32, i32, i32)>] {
        &self.strokes
    }

    pub fn clear(&mut self) {
        self.strokes.clear();
        self.current.clear();
        self.last_erase = None;
        self.bbox = BBox::empty();
    }

    /// Pen touched down or moved while down, with brush radius already
    /// resolved by the caller. Returns the dirty rect of what was drawn.
    pub fn pen_point(&mut self, surf: &mut Surface, x: i32, y: i32, r: i32) -> BBox {
        let radius = smoothing::smoothed_radius(&self.current, r);
        let point = (x, y, radius);
        let dirty = smoothing::draw_live_point(surf, &self.current, point);
        self.current.push(point);
        self.bbox.add(x, y, radius + 2);
        dirty
    }

    /// Eraser tip: brush white over the page AND drop the stored points it
    /// covers, so the stroke model stays true to the visible page. Without
    /// this, erased ink would still be remembered and re-conjured, and an
    /// erased "?" would still summon the guide.
    pub fn erase_point(&mut self, surf: &mut Surface, x: i32, y: i32, r: i32) -> BBox {
        let mut dirty = BBox::empty();
        if let Some((px, py)) = self.last_erase {
            surf.brush_line(px, py, x, y, r, WHITE);
            dirty.add(px, py, r + 2);
        } else {
            surf.stamp(x, y, r, WHITE);
        }
        dirty.add(x, y, r + 2);
        self.forget_near(x, y, r);
        self.last_erase = Some((x, y));
        dirty
    }

    /// Remove committed stroke points within `r` of (x, y); split strokes that
    /// are erased through the middle, and recompute the ink bbox.
    fn forget_near(&mut self, x: i32, y: i32, r: i32) {
        let r2 = (r + 2) * (r + 2);
        let mut kept: Vec<Vec<(i32, i32, i32)>> = Vec::new();
        for stroke in self.strokes.drain(..) {
            let mut seg: Vec<(i32, i32, i32)> = Vec::new();
            for p in stroke {
                let (dx, dy) = (p.0 - x, p.1 - y);
                if dx * dx + dy * dy <= r2 {
                    if !seg.is_empty() {
                        kept.push(std::mem::take(&mut seg));
                    }
                } else {
                    seg.push(p);
                }
            }
            if !seg.is_empty() {
                kept.push(seg);
            }
        }
        self.strokes = kept;
        self.bbox = BBox::empty();
        for stroke in &self.strokes {
            for &(px, py, pr) in stroke {
                self.bbox.add(px, py, pr + 2);
            }
        }
    }

    /// Finish the low-latency preview and overlay anti-aliased edges. Returns
    /// the only region that needs a quality waveform submission.
    pub fn pen_up(&mut self, surf: &mut Surface) -> BBox {
        let dirty = smoothing::settle_stroke(surf, &self.current);
        if !self.current.is_empty() {
            self.strokes.push(std::mem::take(&mut self.current));
        }
        self.last_erase = None;
        dirty
    }

    /// Snapshot the ink crop without doing PNG work on the UI thread.
    pub fn capture(&self, surf: &Surface) -> std::io::Result<PageCapture> {
        if self.bbox.is_empty() {
            return Err(std::io::Error::other("no ink"));
        }
        let (bx, by, bw, bh) = self.bbox.rect();
        let x0 = (bx - 20).max(0) as usize;
        let y0 = (by - 20).max(0) as usize;
        let x1 = ((bx + bw + 20) as usize).min(surf.w);
        let y1 = ((by + bh + 20) as usize).min(surf.h);
        let max_edge = std::env::var("MAGICPAPER_IMAGE_MAX_EDGE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| v >= 400)
            .unwrap_or(1600);
        Ok(PageCapture {
            pixels: surf.copy_rect(x0, y0, x1 - x0, y1 - y0),
            width: x1 - x0,
            height: y1 - y0,
            format: surf.fmt,
            max_edge,
        })
    }
}

fn encode_gray_png(gray: &[u8], width: usize, height: usize) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, width as u32, height as u32);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Fast);
        let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
        writer
            .write_image_data(gray)
            .map_err(std::io::Error::other)?;
    }
    Ok(bytes)
}

/// Deterministic per-pixel hash for the dissolve pattern.
#[inline]
fn px_hash(x: i32, y: i32) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E3779B1) ^ (y as u32).wrapping_mul(0x85EBCA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2AE35);
    h ^ (h >> 16)
}

/// One pass of the "diary drinks the ink" effect: erase the pixels whose hash
/// falls in this stage. After `stages` passes the region is clean white.
pub fn dissolve_pass(surf: &mut Surface, region: BBox, stage: u32, stages: u32) {
    assert!(stages > 0, "dissolve requires at least one stage");
    if region.is_empty() {
        return;
    }
    for y in region.y0..=region.y1 {
        for x in region.x0..=region.x1 {
            if surf.luma(x, y) < 250 && px_hash(x, y) % stages <= stage {
                surf.put_px(x, y, WHITE);
            }
        }
    }
}

/// Render one dissolve frame and select the matching monochrome waveform.
/// Intermediate frames prioritize motion; the terminal frame explicitly
/// whites the full dirty rectangle and requests one quality partial cleanup.
pub fn dissolve_frame(surf: &mut Surface, region: BBox, stage: u32, stages: u32) -> RefreshIntent {
    dissolve_pass(surf, region, stage, stages);
    if stage + 1 < stages {
        return RefreshIntent::Ink;
    }
    if !region.is_empty() {
        let (x, y, width, height) = region.rect();
        surf.fill_rect(
            x as usize,
            y as usize,
            width as usize,
            height as usize,
            WHITE,
        );
    }
    RefreshIntent::MonoQuality
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::PixFmt;

    fn surf() -> (Vec<u8>, Surface) {
        crate::fb::test_init_screen();
        let mut buf = vec![0xFFu8; 400 * 400 * 4];
        let ptr = buf.as_mut_ptr();
        let s = Surface::new(ptr, buf.len(), 400, 400, 400 * 4, PixFmt::Rgb32);
        (buf, s)
    }

    #[test]
    fn erase_forgets_covered_points_and_splits_strokes() {
        let (_buf, mut s) = surf();
        let mut ink = Ink::new();
        // A horizontal stroke across the page.
        for x in (20..=200).step_by(10) {
            ink.pen_point(&mut s, x, 100, 3);
        }
        ink.pen_up(&mut s);
        assert_eq!(ink.stroke_list().len(), 1);
        let before: usize = ink.stroke_list().iter().map(|s| s.len()).sum();

        // Erase through the middle: the stroke splits, points vanish.
        ink.erase_point(&mut s, 110, 100, 20);
        let after: usize = ink.stroke_list().iter().map(|s| s.len()).sum();
        assert!(
            after < before,
            "erase kept every point ({after} of {before})"
        );
        assert_eq!(
            ink.stroke_list().len(),
            2,
            "middle-erase should split the stroke"
        );
        // No surviving point lies under the eraser.
        for st in ink.stroke_list() {
            for &(x, y, _) in st {
                assert!((x - 110).pow(2) + (y - 100).pow(2) > 22 * 22);
            }
        }
    }

    #[test]
    fn erasing_everything_empties_the_ink() {
        let (_buf, mut s) = surf();
        let mut ink = Ink::new();
        ink.pen_point(&mut s, 100, 100, 3);
        ink.pen_point(&mut s, 104, 100, 3);
        ink.pen_up(&mut s);
        assert!(!ink.is_empty());
        ink.erase_point(&mut s, 102, 100, 30);
        assert!(ink.stroke_list().is_empty());
        assert!(ink.bbox.is_empty());
    }

    #[test]
    fn capture_encodes_without_retaining_the_live_surface() {
        let (_buf, mut surface) = surf();
        let mut ink = Ink::new();
        ink.pen_point(&mut surface, 100, 100, 3);
        ink.pen_up(&mut surface);
        let capture = ink.capture(&surface).unwrap();
        surface.fill_rect(0, 0, surface.w, surface.h, WHITE);
        let bytes = capture.encode_png(&AtomicBool::new(false)).unwrap();
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn cancelled_capture_never_starts_png_compression() {
        let (_buf, mut surface) = surf();
        let mut ink = Ink::new();
        ink.pen_point(&mut surface, 100, 100, 3);
        ink.pen_up(&mut surface);
        let capture = ink.capture(&surface).unwrap();
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            capture.encode_png(&cancelled).unwrap_err().kind(),
            std::io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn pressure_changes_are_filtered_without_changing_saved_point_geometry() {
        let (_buf, mut surface) = surf();
        let mut ink = Ink::new();
        ink.pen_point(&mut surface, 20, 30, 2);
        ink.pen_point(&mut surface, 40, 35, 8);
        ink.pen_point(&mut surface, 60, 30, 2);
        let settled = ink.pen_up(&mut surface);
        assert_eq!(
            ink.stroke_list()[0]
                .iter()
                .map(|&(x, y, _)| (x, y))
                .collect::<Vec<_>>(),
            vec![(20, 30), (40, 35), (60, 30)]
        );
        assert_eq!(
            ink.stroke_list()[0]
                .iter()
                .map(|&(_, _, radius)| radius)
                .collect::<Vec<_>>(),
            vec![2, 4, 3]
        );
        let (x, y, width, height) = settled.rect();
        assert!(x <= 16 && y <= 26 && width >= 48 && height >= 13);
    }

    #[test]
    fn pen_up_quality_overlay_retains_gray_edge_coverage() {
        let (_buf, mut surface) = surf();
        let mut ink = Ink::new();
        ink.pen_point(&mut surface, 20, 100, 2);
        ink.pen_point(&mut surface, 100, 103, 2);
        ink.pen_point(&mut surface, 180, 100, 2);
        ink.pen_up(&mut surface);
        assert!(surface.luma(100, 102) < 10);
        assert!((95..=108).any(|y| { (20..=180).any(|x| (1..=254).contains(&surface.luma(x, y))) }));
        assert_eq!(surface.luma(100, 112), 255);
    }

    #[test]
    fn long_stroke_settling_scales_with_the_local_path() {
        let (_buf, mut surface) = surf();
        let mut ink = Ink::new();
        for sample in 0..1_200 {
            let x = 30 + sample / 4;
            let y = 200 + ((sample / 24) % 2) * 3;
            ink.pen_point(&mut surface, x, y, 3);
        }
        let started = std::time::Instant::now();
        let damage = ink.pen_up(&mut surface);
        let elapsed = started.elapsed();
        eprintln!(
            "magic-paper-test: settled_points=1200 elapsed_us={}",
            elapsed.as_micros()
        );
        assert!(!damage.is_empty());
        assert!(damage.rect().2 < 340, "settling expanded to panel width");
        assert!(elapsed < std::time::Duration::from_millis(200));
    }

    fn terminal_dissolve_is_local_and_quality_monochrome(format: PixFmt) {
        let bytes_per_pixel = match format {
            PixFmt::Rgb565 => 2,
            PixFmt::Rgb32 => 4,
        };
        let mut buffer = vec![0xff; 20 * 20 * bytes_per_pixel];
        let mut surface = Surface::new(
            buffer.as_mut_ptr(),
            buffer.len(),
            20,
            20,
            20 * bytes_per_pixel,
            format,
        );
        let region = BBox {
            x0: 5,
            y0: 6,
            x1: 14,
            y1: 13,
        };
        surface.fill_rect(5, 6, 10, 8, BLACK);
        surface.put_px(1, 1, BLACK);

        for stage in 0..3 {
            assert_eq!(
                dissolve_frame(&mut surface, region, stage, 4),
                RefreshIntent::Ink
            );
        }
        assert_eq!(
            dissolve_frame(&mut surface, region, 3, 4),
            RefreshIntent::MonoQuality
        );
        for y in 6..=13 {
            for x in 5..=14 {
                assert_eq!(surface.luma(x, y), 255);
            }
        }
        assert_eq!(surface.luma(1, 1), 0, "cleanup escaped its dirty rect");
    }

    #[test]
    fn terminal_dissolve_cleans_rgb565_without_touching_the_rest_of_the_page() {
        terminal_dissolve_is_local_and_quality_monochrome(PixFmt::Rgb565);
    }

    #[test]
    fn terminal_dissolve_cleans_rgb32_without_touching_the_rest_of_the_page() {
        terminal_dissolve_is_local_and_quality_monochrome(PixFmt::Rgb32);
    }
}
