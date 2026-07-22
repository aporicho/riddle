//! Geometry helpers. Drawing lives in surface.rs.

use std::sync::OnceLock;

static SCREEN_DIMS: OnceLock<(usize, usize)> = OnceLock::new();

pub fn init_screen(w: usize, h: usize) {
    let _ = SCREEN_DIMS.set((w, h));
}

pub fn screen_w() -> usize {
    SCREEN_DIMS
        .get()
        .expect("init_screen not called before screen_w")
        .0
}

pub fn screen_h() -> usize {
    SCREEN_DIMS
        .get()
        .expect("init_screen not called before screen_h")
        .1
}

#[cfg(test)]
pub fn test_init_screen() {
    // Paper Pro Move's real logical qtfb canvas.  Layout tests must exercise
    // the narrow device rather than silently passing on a much larger page.
    init_screen(954, 1696);
}

/// Grow-only pixel bounding box, used to build update/dissolve regions.
#[derive(Clone, Copy, Debug)]
pub struct BBox {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl BBox {
    pub fn empty() -> Self {
        Self {
            x0: i32::MAX,
            y0: i32::MAX,
            x1: i32::MIN,
            y1: i32::MIN,
        }
    }
    pub fn is_empty(&self) -> bool {
        self.x0 > self.x1 || self.y0 > self.y1
    }
    pub fn add(&mut self, x: i32, y: i32, margin: i32) {
        self.x0 = self.x0.min(x - margin).max(0);
        self.y0 = self.y0.min(y - margin).max(0);
        self.x1 = self.x1.max(x + margin).min(screen_w() as i32 - 1);
        self.y1 = self.y1.max(y + margin).min(screen_h() as i32 - 1);
    }
    pub fn rect(&self) -> (i32, i32, i32, i32) {
        (
            self.x0,
            self.y0,
            self.x1 - self.x0 + 1,
            self.y1 - self.y0 + 1,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::BBox;

    #[test]
    fn either_invalid_axis_makes_a_box_empty() {
        assert!(BBox {
            x0: 0,
            x1: 10,
            y0: 20,
            y1: 10,
        }
        .is_empty());
        assert!(BBox {
            x0: 10,
            x1: 0,
            y0: 0,
            y1: 20,
        }
        .is_empty());
    }
}
