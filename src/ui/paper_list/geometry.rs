use crate::ui::pointer::{HitRect, Point};

pub(super) fn segment_intersects_rect(from: Point, to: Point, rect: HitRect) -> bool {
    if rect.contains(from) || rect.contains(to) || rect.width() <= 0 || rect.height() <= 0 {
        return rect.contains(from) || rect.contains(to);
    }
    let top_left = Point::new(rect.x0, rect.y0);
    let top_right = Point::new(rect.x1 - 1, rect.y0);
    let bottom_left = Point::new(rect.x0, rect.y1 - 1);
    let bottom_right = Point::new(rect.x1 - 1, rect.y1 - 1);
    [
        (top_left, top_right),
        (top_right, bottom_right),
        (bottom_right, bottom_left),
        (bottom_left, top_left),
    ]
    .into_iter()
    .any(|(edge_start, edge_end)| segments_intersect(from, to, edge_start, edge_end))
}

fn segments_intersect(a: Point, b: Point, c: Point, d: Point) -> bool {
    let ab_c = orientation(a, b, c);
    let ab_d = orientation(a, b, d);
    let cd_a = orientation(c, d, a);
    let cd_b = orientation(c, d, b);
    if ab_c == 0 && on_segment(a, b, c)
        || ab_d == 0 && on_segment(a, b, d)
        || cd_a == 0 && on_segment(c, d, a)
        || cd_b == 0 && on_segment(c, d, b)
    {
        return true;
    }
    (ab_c > 0) != (ab_d > 0) && (cd_a > 0) != (cd_b > 0)
}

fn orientation(a: Point, b: Point, c: Point) -> i64 {
    (b.x - a.x) as i64 * (c.y - a.y) as i64 - (b.y - a.y) as i64 * (c.x - a.x) as i64
}

fn on_segment(a: Point, b: Point, point: Point) -> bool {
    point.x >= a.x.min(b.x)
        && point.x <= a.x.max(b.x)
        && point.y >= a.y.min(b.y)
        && point.y <= a.y.max(b.y)
}
