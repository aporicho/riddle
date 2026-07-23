//! High-resolution glyph skeletons converted into smooth animated pen paths.

use crate::fonts::FontBook;

use super::{rasterize_line, thin, trace};

/// A supersampled glyph skeleton converted back to panel-space vector paths.
/// Floating-point coordinates are intentionally retained until compositing so
/// the e-ink quality pass can preserve sub-pixel edge coverage.
pub struct HandwritingLine {
    pub width: usize,
    pub height: usize,
    pub strokes: Vec<Vec<(f32, f32)>>,
}

// 1.5x retains fractional geometry and removes the 1x stair-step while keeping
// CJK thinning within the first-ink latency budget on the tablet CPU. 2x costs
// roughly twice as much again because both mask area and thinning passes grow.
const HANDWRITING_SUPERSAMPLE: f32 = 1.5;

fn perpendicular_distance(point: (f32, f32), start: (f32, f32), end: (f32, f32)) -> f32 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    let length_sq = dx * dx + dy * dy;
    if length_sq <= f32::EPSILON {
        return ((point.0 - start.0).powi(2) + (point.1 - start.1).powi(2)).sqrt();
    }
    let cross = (dy * point.0 - dx * point.1 + end.0 * start.1 - end.1 * start.0).abs();
    cross / length_sq.sqrt()
}

/// Iterative Ramer-Douglas-Peucker simplification. Keeping it non-recursive
/// avoids growing the stack on long CJK skeletons.
pub(super) fn simplify_path(points: &[(f32, f32)], epsilon: f32) -> Vec<(f32, f32)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut ranges = vec![(0, points.len() - 1)];
    while let Some((start, end)) = ranges.pop() {
        let mut furthest = None;
        let mut distance = epsilon;
        for index in start + 1..end {
            let candidate = perpendicular_distance(points[index], points[start], points[end]);
            if candidate > distance {
                distance = candidate;
                furthest = Some(index);
            }
        }
        if let Some(index) = furthest {
            keep[index] = true;
            ranges.push((start, index));
            ranges.push((index, end));
        }
    }
    points
        .iter()
        .zip(keep)
        .filter_map(|(&point, keep)| keep.then_some(point))
        .collect()
}

fn chaikin(points: &[(f32, f32)]) -> Vec<(f32, f32)> {
    if points.len() <= 2 {
        return points.to_vec();
    }
    let mut smoothed = Vec::with_capacity(points.len() * 2);
    smoothed.push(points[0]);
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        smoothed.push((a.0 * 0.75 + b.0 * 0.25, a.1 * 0.75 + b.1 * 0.25));
        smoothed.push((a.0 * 0.25 + b.0 * 0.75, a.1 * 0.25 + b.1 * 0.75));
    }
    smoothed.push(*points.last().expect("nonempty smoothed path"));
    smoothed
}

/// Resample by geometric length so the animation budget represents visible
/// pen travel rather than the number of pixels left by skeletonization.
pub(super) fn resample_path(points: &[(f32, f32)], spacing: f32) -> Vec<(f32, f32)> {
    let Some(&first) = points.first() else {
        return Vec::new();
    };
    let mut sampled = vec![first];
    for pair in points.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let distance = ((end.0 - start.0).powi(2) + (end.1 - start.1).powi(2)).sqrt();
        let steps = (distance / spacing).ceil().max(1.0) as usize;
        for index in 1..=steps {
            let t = index as f32 / steps as f32;
            let point = (
                start.0 + (end.0 - start.0) * t,
                start.1 + (end.1 - start.1) * t,
            );
            if sampled.last().is_none_or(|last| {
                (last.0 - point.0).abs() > 0.01 || (last.1 - point.1).abs() > 0.01
            }) {
                sampled.push(point);
            }
        }
    }
    sampled
}

/// Rasterize at a higher resolution, skeletonize there, then return simplified
/// and smoothed panel-space paths. This keeps the handwritten character shape
/// while eliminating the square stair-steps of a 1x boolean skeleton.
pub fn trace_handwriting(fonts: &FontBook, text: &str, px: f32) -> HandwritingLine {
    let mut high = rasterize_line(fonts, text, px * HANDWRITING_SUPERSAMPLE);
    thin(&mut high);
    let strokes = trace(&high)
        .into_iter()
        .filter_map(|stroke| {
            let panel_points: Vec<_> = stroke
                .into_iter()
                .map(|(x, y)| {
                    (
                        x as f32 / HANDWRITING_SUPERSAMPLE,
                        y as f32 / HANDWRITING_SUPERSAMPLE,
                    )
                })
                .collect();
            let simplified = simplify_path(&panel_points, 0.7);
            let smoothed = chaikin(&simplified);
            let sampled = resample_path(&smoothed, 1.25);
            (sampled.len() >= 2).then_some(sampled)
        })
        .collect();
    HandwritingLine {
        width: (high.width as f32 / HANDWRITING_SUPERSAMPLE).ceil() as usize,
        height: (high.height as f32 / HANDWRITING_SUPERSAMPLE).ceil() as usize,
        strokes,
    }
}
