//! Tool-aware contact classification and shared hit-test geometry for modal UI.
//!
//! Runtime code owns device tracking and supplies elapsed time. Components may
//! also receive an already classified [`Gesture`] directly. Keeping the tool on
//! every gesture makes destructive pen-only actions impossible to trigger by
//! accidentally treating a finger drag as ink.

use std::time::Duration;

use crate::surface::{Surface, BLACK, WHITE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

impl Point {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HitRect {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
}

impl HitRect {
    /// A rectangle with exclusive right and bottom edges.
    pub fn from_xywh(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x0: x,
            y0: y,
            x1: x.saturating_add(width.max(0)),
            y1: y.saturating_add(height.max(0)),
        }
    }

    pub fn from_points(points: &[Point]) -> Option<Self> {
        let first = *points.first()?;
        let mut rect = Self::from_xywh(first.x, first.y, 1, 1);
        for point in &points[1..] {
            rect.x0 = rect.x0.min(point.x);
            rect.y0 = rect.y0.min(point.y);
            rect.x1 = rect.x1.max(point.x.saturating_add(1));
            rect.y1 = rect.y1.max(point.y.saturating_add(1));
        }
        Some(rect)
    }

    pub const fn width(self) -> i32 {
        self.x1.saturating_sub(self.x0)
    }

    pub const fn height(self) -> i32 {
        self.y1.saturating_sub(self.y0)
    }

    pub const fn contains(self, point: Point) -> bool {
        point.x >= self.x0 && point.x < self.x1 && point.y >= self.y0 && point.y < self.y1
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.x0 < other.x1 && self.x1 > other.x0 && self.y0 < other.y1 && self.y1 > other.y0
    }

    /// Clip a component-owned hit rectangle to the actual framebuffer.
    /// Preview code snapshots exactly this rectangle before drawing feedback,
    /// so leaving a control can restore the original pixels byte-for-byte.
    pub fn clipped_to(self, width: usize, height: usize) -> Option<Self> {
        let clipped = Self {
            x0: self.x0.clamp(0, width as i32),
            y0: self.y0.clamp(0, height as i32),
            x1: self.x1.clamp(0, width as i32),
            y1: self.y1.clamp(0, height as i32),
        };
        (clipped.width() > 0 && clipped.height() > 0).then_some(clipped)
    }
}

/// High-contrast pressed feedback for monochrome controls. Runtime software
/// layers restore the clean UI pixels when the pointer leaves or lifts.
pub fn invert_mono(surface: &mut Surface, rect: HitRect) {
    let Some(rect) = rect.clipped_to(surface.w, surface.h) else {
        return;
    };
    for y in rect.y0..rect.y1 {
        for x in rect.x0..rect.x1 {
            let color = if surface.luma(x, y) < 128 {
                WHITE
            } else {
                BLACK
            };
            surface.put_px(x, y, color);
        }
    }
}

/// Draw a temporary line without clipping to component geometry. The target
/// surface still clips at the framebuffer boundary.
pub fn draw_line(surface: &mut Surface, from: Point, to: Point, radius: i32) {
    let dx = (to.x - from.x).abs();
    let dy = (to.y - from.y).abs();
    let steps = dx.max(dy).max(1);
    for step in 0..=steps {
        let x = from.x + (to.x - from.x) * step / steps;
        let y = from.y + (to.y - from.y) * step / steps;
        surface.stamp(x, y, radius, BLACK);
    }
}

/// Draw a temporary line without ever escaping the component-provided hit
/// rectangle. In particular this avoids turning clipped modal feedback into
/// page ink.
#[cfg_attr(not(test), allow(dead_code))]
pub fn draw_clipped_line(
    surface: &mut Surface,
    from: Point,
    to: Point,
    clip: HitRect,
    radius: i32,
) {
    let Some(clip) = clip.clipped_to(surface.w, surface.h) else {
        return;
    };
    let dx = (to.x - from.x).abs();
    let dy = (to.y - from.y).abs();
    let steps = dx.max(dy).max(1);
    for step in 0..=steps {
        let x = from.x + (to.x - from.x) * step / steps;
        let y = from.y + (to.y - from.y) * step / steps;
        for oy in -radius..=radius {
            for ox in -radius..=radius {
                if ox * ox + oy * oy <= radius * radius {
                    let point = Point::new(x + ox, y + oy);
                    if clip.contains(point) {
                        surface.put_px(point.x, point.y, BLACK);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerTool {
    Pen,
    Finger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gesture {
    Tap {
        tool: PointerTool,
        at: Point,
    },
    Swipe {
        tool: PointerTool,
        from: Point,
        to: Point,
        bounds: HitRect,
    },
    Strike {
        tool: PointerTool,
        from: Point,
        to: Point,
        bounds: HitRect,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContactProfile {
    pub tap_slop_px: i32,
    pub tap_max_duration: Duration,
    pub swipe_min_distance_px: i32,
    pub strike_min_distance_px: i32,
    /// Primary-axis distance must be at least this multiple of the other axis.
    pub axis_dominance: i32,
}

impl ContactProfile {
    pub const fn sane(self) -> bool {
        self.tap_slop_px >= 0
            && !self.tap_max_duration.is_zero()
            && self.swipe_min_distance_px > self.tap_slop_px
            && self.strike_min_distance_px > self.tap_slop_px
            && self.axis_dominance >= 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GesturePolicy {
    pub pen: ContactProfile,
    pub finger: ContactProfile,
}

impl Default for GesturePolicy {
    fn default() -> Self {
        Self {
            pen: ContactProfile {
                tap_slop_px: 18,
                tap_max_duration: Duration::from_millis(350),
                swipe_min_distance_px: 96,
                strike_min_distance_px: 120,
                axis_dominance: 2,
            },
            finger: ContactProfile {
                tap_slop_px: 32,
                tap_max_duration: Duration::from_millis(500),
                swipe_min_distance_px: 120,
                strike_min_distance_px: 150,
                axis_dominance: 2,
            },
        }
    }
}

impl GesturePolicy {
    pub fn classify(
        self,
        tool: PointerTool,
        points: &[Point],
        duration: Duration,
    ) -> Option<Gesture> {
        let profile = match tool {
            PointerTool::Pen => self.pen,
            PointerTool::Finger => self.finger,
        };
        if !profile.sane() {
            return None;
        }
        let bounds = HitRect::from_points(points)?;
        let from = *points.first()?;
        let to = *points.last()?;
        if duration <= profile.tap_max_duration
            && bounds.width().saturating_sub(1) <= profile.tap_slop_px
            && bounds.height().saturating_sub(1) <= profile.tap_slop_px
        {
            return Some(Gesture::Tap { tool, at: to });
        }
        let dx = (to.x - from.x).abs();
        let dy = (to.y - from.y).abs();
        if tool == PointerTool::Pen
            && dx >= profile.strike_min_distance_px
            && dx >= dy.saturating_mul(profile.axis_dominance)
            && bounds.width() >= bounds.height().saturating_mul(profile.axis_dominance)
        {
            Some(Gesture::Strike {
                tool,
                from,
                to,
                bounds,
            })
        } else if dx.max(dy) >= profile.swipe_min_distance_px {
            Some(Gesture::Swipe {
                tool,
                from,
                to,
                bounds,
            })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temporary_line_is_clipped_to_component_geometry() {
        let mut pixels = vec![0xFF; 64 * 64 * 4];
        let mut surface = Surface::new(
            pixels.as_mut_ptr(),
            pixels.len(),
            64,
            64,
            64 * 4,
            crate::surface::PixFmt::Rgb32,
        );
        let clip = HitRect::from_xywh(20, 20, 20, 10);
        draw_clipped_line(&mut surface, Point::new(5, 25), Point::new(55, 25), clip, 2);
        assert_eq!(surface.luma(19, 25), 255);
        assert_eq!(surface.luma(20, 25), 0);
        assert_eq!(surface.luma(39, 25), 0);
        assert_eq!(surface.luma(40, 25), 255);
    }

    #[test]
    fn temporary_free_line_crosses_component_geometry() {
        let mut pixels = vec![0xFF; 64 * 64 * 4];
        let mut surface = Surface::new(
            pixels.as_mut_ptr(),
            pixels.len(),
            64,
            64,
            64 * 4,
            crate::surface::PixFmt::Rgb32,
        );
        draw_line(&mut surface, Point::new(5, 25), Point::new(55, 25), 2);
        assert_eq!(surface.luma(5, 25), 0);
        assert_eq!(surface.luma(55, 25), 0);
    }

    #[test]
    fn tap_uses_tool_specific_slop_and_duration() {
        let policy = GesturePolicy::default();
        let points = [Point::new(10, 10), Point::new(26, 12)];
        assert!(matches!(
            policy.classify(PointerTool::Pen, &points, Duration::from_millis(300)),
            Some(Gesture::Tap { .. })
        ));
        assert!(policy
            .classify(PointerTool::Pen, &points, Duration::from_millis(500))
            .is_none());
    }

    #[test]
    fn movement_must_clear_configured_swipe_distance() {
        let policy = GesturePolicy::default();
        let points = [Point::new(10, 10), Point::new(10, 80)];
        assert_eq!(
            policy.classify(PointerTool::Pen, &points, Duration::from_millis(500)),
            None
        );
        let points = [Point::new(10, 10), Point::new(10, 130)];
        assert!(matches!(
            policy.classify(PointerTool::Pen, &points, Duration::from_millis(500)),
            Some(Gesture::Swipe { .. })
        ));
    }

    #[test]
    fn only_pen_contacts_are_classified_as_strikes() {
        let policy = GesturePolicy::default();
        let points = [Point::new(10, 50), Point::new(300, 54)];
        assert!(matches!(
            policy.classify(PointerTool::Pen, &points, Duration::from_millis(700)),
            Some(Gesture::Strike { .. })
        ));
        assert!(matches!(
            policy.classify(PointerTool::Finger, &points, Duration::from_millis(700)),
            Some(Gesture::Swipe { .. })
        ));
    }
}
