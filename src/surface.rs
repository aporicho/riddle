//! Drawing surface abstraction: same drawing code renders into either the
//! qtfb RGB565 shared memory (in-xochitl backend) or the vendor engine's
//! RGB32 aux framebuffer (takeover backend). Colors are RGB565 u16 at the
//! API; the surface converts on write.

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PixFmt {
    /// 2 bytes/px, little-endian RGB565 (device-specific QTFB format).
    Rgb565,
    /// 4 bytes/px, QImage Format_RGB32: bytes B,G,R,0xFF.
    #[cfg_attr(
        not(any(feature = "takeover", test)),
        expect(dead_code, reason = "constructed only by takeover and test surfaces")
    )]
    Rgb32,
}

pub struct Surface {
    ptr: *mut u8,
    len: usize,
    pub w: usize,
    pub h: usize,
    pub stride: usize,
    pub fmt: PixFmt,
}

// Single-threaded writer over a long-lived mapping.
unsafe impl Send for Surface {}

pub const WHITE: u16 = 0xFFFF;
pub const BLACK: u16 = 0x0000;
/// Old ink: how the diary writes its memories (a readable e-ink gray).
pub const FADED: u16 = 0x7BCF;

pub struct OwnedSurface {
    bytes: Vec<u8>,
    w: usize,
    h: usize,
    stride: usize,
    fmt: PixFmt,
}

impl OwnedSurface {
    pub fn new_like(template: &Surface, fill: u16) -> Self {
        let mut surface = Self {
            bytes: vec![0; template.stride * template.h],
            w: template.w,
            h: template.h,
            stride: template.stride,
            fmt: template.fmt,
        };
        surface
            .as_surface()
            .fill_rect(0, 0, template.w, template.h, fill);
        surface
    }

    pub fn as_surface(&mut self) -> Surface {
        Surface::new(
            self.bytes.as_mut_ptr(),
            self.bytes.len(),
            self.w,
            self.h,
            self.stride,
            self.fmt,
        )
    }

    pub fn copy_from_surface(&mut self, source: &Surface, rect: crate::ui::pointer::HitRect) {
        let Some(rect) = rect.clipped_to(self.w, self.h) else {
            return;
        };
        let pixels = source.copy_rect(
            rect.x0 as usize,
            rect.y0 as usize,
            rect.width() as usize,
            rect.height() as usize,
        );
        self.as_surface().paste_rect(
            rect.x0 as usize,
            rect.y0 as usize,
            rect.width() as usize,
            rect.height() as usize,
            &pixels,
        );
    }

    pub fn copy_to_surface(&mut self, target: &mut Surface, rect: crate::ui::pointer::HitRect) {
        let Some(rect) = rect.clipped_to(self.w, self.h) else {
            return;
        };
        let pixels = self.as_surface().copy_rect(
            rect.x0 as usize,
            rect.y0 as usize,
            rect.width() as usize,
            rect.height() as usize,
        );
        target.paste_rect(
            rect.x0 as usize,
            rect.y0 as usize,
            rect.width() as usize,
            rect.height() as usize,
            &pixels,
        );
    }
}

#[inline]
fn expand565(c: u16) -> (u8, u8, u8) {
    let r = ((c >> 11) & 0x1f) as u32;
    let g = ((c >> 5) & 0x3f) as u32;
    let b = (c & 0x1f) as u32;
    (
        ((r * 255 + 15) / 31) as u8,
        ((g * 255 + 31) / 63) as u8,
        ((b * 255 + 15) / 31) as u8,
    )
}

impl Surface {
    pub fn new(ptr: *mut u8, len: usize, w: usize, h: usize, stride: usize, fmt: PixFmt) -> Self {
        Self {
            ptr,
            len,
            w,
            h,
            stride,
            fmt,
        }
    }

    #[inline]
    fn buf(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    #[inline]
    fn buf_ref(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    #[inline]
    pub fn put_px(&mut self, x: i32, y: i32, c: u16) {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        let (stride, fmt) = (self.stride, self.fmt);
        match fmt {
            PixFmt::Rgb565 => {
                let i = y as usize * stride + x as usize * 2;
                let b = self.buf();
                b[i] = (c & 0xff) as u8;
                b[i + 1] = (c >> 8) as u8;
            }
            PixFmt::Rgb32 => {
                let (r, g, bl) = expand565(c);
                let i = y as usize * stride + x as usize * 4;
                let b = self.buf();
                b[i] = bl;
                b[i + 1] = g;
                b[i + 2] = r;
                b[i + 3] = 0xFF;
            }
        }
    }

    /// Luminance 0..255 — used by the PNG rasterizer and dissolve inkness test.
    #[inline]
    pub fn luma(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return 255;
        }
        let b = self.buf_ref();
        match self.fmt {
            PixFmt::Rgb565 => {
                let i = y as usize * self.stride + x as usize * 2;
                let px = (b[i] as u16) | ((b[i + 1] as u16) << 8);
                (((px >> 5) & 0x3f) as u32 * 255 / 63) as u8
            }
            PixFmt::Rgb32 => {
                let i = y as usize * self.stride + x as usize * 4;
                // Green approximates luma well enough for mono ink.
                b[i + 1]
            }
        }
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: u16) {
        let x1 = (x + w).min(self.w);
        let y1 = (y + h).min(self.h);
        for row in y..y1 {
            for col in x..x1 {
                self.put_px(col as i32, row as i32, c);
            }
        }
    }

    #[inline]
    fn bpp(&self) -> usize {
        match self.fmt {
            PixFmt::Rgb565 => 2,
            PixFmt::Rgb32 => 4,
        }
    }

    /// Snapshot a rect's raw bytes (for save-under panels).
    pub fn copy_rect(&self, x: usize, y: usize, w: usize, h: usize) -> Vec<u8> {
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        let bpp = self.bpp();
        let b = self.buf_ref();
        let mut out = Vec::with_capacity((x1 - x) * (y1 - y) * bpp);
        for row in y..y1 {
            let s = row * self.stride + x * bpp;
            out.extend_from_slice(&b[s..s + (x1 - x) * bpp]);
        }
        out
    }

    /// Put back bytes captured by `copy_rect` with the same geometry.
    pub fn paste_rect(&mut self, x: usize, y: usize, w: usize, h: usize, data: &[u8]) {
        let (x1, y1) = ((x + w).min(self.w), (y + h).min(self.h));
        let (bpp, stride) = (self.bpp(), self.stride);
        let row_len = (x1 - x) * bpp;
        let b = self.buf();
        for (i, row) in (y..y1).enumerate() {
            let s = row * stride + x * bpp;
            b[s..s + row_len].copy_from_slice(&data[i * row_len..(i + 1) * row_len]);
        }
    }

    pub fn stamp(&mut self, cx: i32, cy: i32, r: i32, c: u16) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    self.put_px(cx + dx, cy + dy, c);
                }
            }
        }
    }

    /// Alpha-composite one RGB565 colour over the current pixel.  Keeping the
    /// blend at the surface boundary gives both framebuffer formats identical
    /// anti-aliased edges instead of making callers guess their byte layout.
    #[inline]
    pub fn blend_px(&mut self, x: i32, y: i32, c: u16, alpha: u8) {
        if alpha == 0 || x < 0 || y < 0 || x >= self.w as i32 || y >= self.h as i32 {
            return;
        }
        if alpha == u8::MAX {
            self.put_px(x, y, c);
            return;
        }
        let (sr, sg, sb) = expand565(c);
        let inverse = u8::MAX - alpha;
        let blend = |dst: u8, src: u8| -> u8 {
            ((dst as u16 * inverse as u16 + src as u16 * alpha as u16 + 127) / 255) as u8
        };
        let (stride, fmt) = (self.stride, self.fmt);
        match fmt {
            PixFmt::Rgb565 => {
                let i = y as usize * stride + x as usize * 2;
                let bytes = self.buf();
                let old = bytes[i] as u16 | (bytes[i + 1] as u16) << 8;
                let (dr, dg, db) = expand565(old);
                let (r, g, b) = (blend(dr, sr), blend(dg, sg), blend(db, sb));
                let packed = ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3);
                bytes[i] = packed as u8;
                bytes[i + 1] = (packed >> 8) as u8;
            }
            PixFmt::Rgb32 => {
                let i = y as usize * stride + x as usize * 4;
                let bytes = self.buf();
                bytes[i] = blend(bytes[i], sb);
                bytes[i + 1] = blend(bytes[i + 1], sg);
                bytes[i + 2] = blend(bytes[i + 2], sr);
                bytes[i + 3] = 0xff;
            }
        }
    }

    /// Apply geometric coverage without accumulating darkness where adjacent
    /// short segments overlap. For black ink the darkest requested coverage
    /// wins, which is equivalent to rasterizing the union of all brush shapes
    /// into one local mask before compositing it onto white paper.
    #[inline]
    fn cover_px(&mut self, x: i32, y: i32, c: u16, coverage: u8) {
        if c != BLACK {
            self.blend_px(x, y, c, coverage);
            return;
        }
        let target = u8::MAX - coverage;
        if self.luma(x, y) <= target {
            return;
        }
        let gray =
            ((target as u16 >> 3) << 11) | ((target as u16 >> 2) << 5) | (target as u16 >> 3);
        self.put_px(x, y, gray);
    }

    /// Draw a round-capped, anti-aliased segment with sub-pixel endpoints.
    /// Work is bounded by the segment's local box, so a pen-up quality pass
    /// never allocates or scans a full-panel supersampling buffer.
    pub fn brush_line_aa(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, radius: f32, c: u16) {
        let radius = radius.max(0.5);
        let fringe = radius + 0.5;
        let min_x = (x0.min(x1) - fringe).floor() as i32;
        let max_x = (x0.max(x1) + fringe).ceil() as i32;
        let min_y = (y0.min(y1) - fringe).floor() as i32;
        let max_y = (y0.max(y1) + fringe).ceil() as i32;
        let dx = x1 - x0;
        let dy = y1 - y0;
        let length_sq = dx * dx + dy * dy;
        for y in min_y..=max_y {
            for x in min_x..=max_x {
                let px = x as f32 + 0.5;
                let py = y as f32 + 0.5;
                let t = if length_sq <= f32::EPSILON {
                    0.0
                } else {
                    (((px - x0) * dx + (py - y0) * dy) / length_sq).clamp(0.0, 1.0)
                };
                let nearest_x = x0 + t * dx;
                let nearest_y = y0 + t * dy;
                let distance = ((px - nearest_x).powi(2) + (py - nearest_y).powi(2)).sqrt();
                let coverage = (fringe - distance).clamp(0.0, 1.0);
                if coverage > 0.0 {
                    self.cover_px(x, y, c, (coverage * 255.0).round() as u8);
                }
            }
        }
    }

    pub fn brush_line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, r: i32, c: u16) {
        let dx = (x1 - x0).abs();
        let dy = (y1 - y0).abs();
        let steps = dx.max(dy).max(1);
        for i in 0..=steps {
            let x = x0 + (x1 - x0) * i / steps;
            let y = y0 + (y1 - y0) * i / steps;
            self.stamp(x, y, r, c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antialiased_segment_has_black_core_and_gray_fringe_in_both_formats() {
        for (format, bpp) in [(PixFmt::Rgb565, 2), (PixFmt::Rgb32, 4)] {
            let mut bytes = vec![0xff; 24 * 24 * bpp];
            let mut surface =
                Surface::new(bytes.as_mut_ptr(), bytes.len(), 24, 24, 24 * bpp, format);
            surface.brush_line_aa(3.25, 12.25, 20.75, 12.25, 2.25, BLACK);
            assert!(surface.luma(10, 12) < 10, "core was not opaque");
            assert!(
                (1..=254).contains(&surface.luma(10, 14)),
                "edge did not retain grayscale coverage"
            );
            assert_eq!(surface.luma(10, 16), 255, "AA escaped its local fringe");

            let fringe = surface.luma(10, 14);
            for _ in 0..8 {
                surface.brush_line_aa(3.25, 12.25, 20.75, 12.25, 2.25, BLACK);
            }
            assert_eq!(
                surface.luma(10, 14),
                fringe,
                "overlapping path segments accumulated fringe darkness"
            );
        }
    }
}
