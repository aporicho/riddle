//! Local geometry for low-latency pen previews and pen-up quality settling.

use crate::fb::BBox;
use crate::surface::{Surface, BLACK};

pub(super) type InkPoint = (i32, i32, i32);

fn midpoint(a: InkPoint, b: InkPoint) -> (f32, f32, f32) {
    (
        (a.0 + b.0) as f32 * 0.5,
        (a.1 + b.1) as f32 * 0.5,
        (a.2 + b.2) as f32 * 0.5,
    )
}

fn quadratic(a: (f32, f32, f32), b: InkPoint, c: (f32, f32, f32), t: f32) -> (f32, f32, f32) {
    let inverse = 1.0 - t;
    let weighted = |start: f32, control: f32, end: f32| {
        inverse * inverse * start + 2.0 * inverse * t * control + t * t * end
    };
    (
        weighted(a.0, b.0 as f32, c.0),
        weighted(a.1, b.1 as f32, c.1),
        weighted(a.2, b.2 as f32, c.2),
    )
}

fn quadratic_steps(a: (f32, f32, f32), b: InkPoint, c: (f32, f32, f32)) -> usize {
    let distance =
        |x0: f32, y0: f32, x1: f32, y1: f32| ((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt();
    ((distance(a.0, a.1, b.0 as f32, b.1 as f32) + distance(b.0 as f32, b.1 as f32, c.0, c.1))
        / 2.0)
        .ceil()
        .max(1.0) as usize
}

fn draw_live_quadratic(
    surf: &mut Surface,
    start: (f32, f32, f32),
    control: InkPoint,
    end: (f32, f32, f32),
) {
    let steps = quadratic_steps(start, control, end);
    let mut previous = start;
    for index in 1..=steps {
        let point = quadratic(start, control, end, index as f32 / steps as f32);
        surf.brush_line(
            previous.0.round() as i32,
            previous.1.round() as i32,
            point.0.round() as i32,
            point.1.round() as i32,
            ((previous.2 + point.2) * 0.5).round().max(1.0) as i32,
            BLACK,
        );
        previous = point;
    }
}

fn draw_quality_quadratic(
    surf: &mut Surface,
    start: (f32, f32, f32),
    control: InkPoint,
    end: (f32, f32, f32),
) {
    let steps = quadratic_steps(start, control, end);
    let mut previous = start;
    for index in 1..=steps {
        let point = quadratic(start, control, end, index as f32 / steps as f32);
        surf.brush_line_aa(
            previous.0,
            previous.1,
            point.0,
            point.1,
            (previous.2 + point.2) * 0.5,
            BLACK,
        );
        previous = point;
    }
}

pub(super) fn smoothed_radius(current: &[InkPoint], radius: i32) -> i32 {
    current
        .last()
        .map_or(radius, |&(_, _, previous)| (previous * 2 + radius + 1) / 3)
        .max(1)
}

/// Draw only the newly confirmed part of an online quadratic path. The first
/// sample is visible immediately; subsequent samples trail by at most half a
/// sample interval while avoiding angular joins.
pub(super) fn draw_live_point(surf: &mut Surface, current: &[InkPoint], point: InkPoint) -> BBox {
    let mut dirty = BBox::empty();
    match current {
        [] => surf.stamp(point.0, point.1, point.2, BLACK),
        [first] => {
            let end = midpoint(*first, point);
            surf.brush_line(
                first.0,
                first.1,
                end.0.round() as i32,
                end.1.round() as i32,
                ((first.2 as f32 + end.2) * 0.5).round() as i32,
                BLACK,
            );
            dirty.add(first.0, first.1, first.2 + 2);
        }
        points => {
            let previous = points[points.len() - 1];
            let before_previous = points[points.len() - 2];
            draw_live_quadratic(
                surf,
                midpoint(before_previous, previous),
                previous,
                midpoint(previous, point),
            );
            dirty.add(previous.0, previous.1, previous.2 + 2);
        }
    }
    dirty.add(point.0, point.1, point.2 + 2);
    dirty
}

/// Draw one completed stroke's smooth quality overlay. The original points
/// remain the persistence/OCR source of truth; only presentation is filtered.
pub(super) fn settle_stroke(surf: &mut Surface, stroke: &[InkPoint]) -> BBox {
    let mut dirty = BBox::empty();
    let Some(&first) = stroke.first() else {
        return dirty;
    };
    dirty.add(first.0, first.1, first.2 + 2);
    if stroke.len() == 1 {
        surf.brush_line_aa(
            first.0 as f32,
            first.1 as f32,
            first.0 as f32,
            first.1 as f32,
            first.2 as f32,
            BLACK,
        );
        return dirty;
    }
    let second = stroke[1];
    let first_mid = midpoint(first, second);
    surf.brush_line_aa(
        first.0 as f32,
        first.1 as f32,
        first_mid.0,
        first_mid.1,
        (first.2 as f32 + first_mid.2) * 0.5,
        BLACK,
    );
    for points in stroke.windows(3) {
        let start = midpoint(points[0], points[1]);
        let end = midpoint(points[1], points[2]);
        draw_quality_quadratic(surf, start, points[1], end);
    }
    let last = stroke[stroke.len() - 1];
    let before_last = stroke[stroke.len() - 2];
    let last_mid = midpoint(before_last, last);
    surf.brush_line_aa(
        last_mid.0,
        last_mid.1,
        last.0 as f32,
        last.1 as f32,
        (last_mid.2 + last.2 as f32) * 0.5,
        BLACK,
    );
    for &(x, y, radius) in stroke {
        dirty.add(x, y, radius + 2);
    }
    dirty
}
