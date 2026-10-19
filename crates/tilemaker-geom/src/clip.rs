//! Clipping lines and polygons to axis-aligned boxes.

use crate::{Coord, LineString, MultiLineString, MultiPolygon, Polygon, Rect};

#[inline]
fn inside(r: &Rect, c: Coord) -> bool {
    c.x >= r.min().x && c.x <= r.max().x && c.y >= r.min().y && c.y <= r.max().y
}

/// Liang–Barsky: the part of segment `a`–`b` inside `r`, if any.
fn clip_segment(r: &Rect, a: Coord, b: Coord) -> Option<(Coord, Coord)> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let mut t0 = 0.0f64;
    let mut t1 = 1.0f64;
    let checks = [
        (-dx, a.x - r.min().x),
        (dx, r.max().x - a.x),
        (-dy, a.y - r.min().y),
        (dy, r.max().y - a.y),
    ];
    for (p, q) in checks {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                if t > t1 {
                    return None;
                }
                t0 = t0.max(t);
            } else {
                if t < t0 {
                    return None;
                }
                t1 = t1.min(t);
            }
        }
    }
    let at = |t: f64| {
        if t == 0.0 {
            a
        } else if t == 1.0 {
            b
        } else {
            Coord {
                x: a.x + t * dx,
                y: a.y + t * dy,
            }
        }
    };
    Some((at(t0), at(t1)))
}

/// Clip a linestring to `r`, splitting it where it leaves the box.
pub fn clip_linestring(ls: &[Coord], r: &Rect, out: &mut MultiLineString) {
    if ls.len() < 2 {
        return;
    }
    if ls.iter().all(|&c| inside(r, c)) {
        out.0.push(LineString::new(ls.to_vec()));
        return;
    }
    let mut current: Vec<Coord> = Vec::new();
    for w in ls.windows(2) {
        match clip_segment(r, w[0], w[1]) {
            Some((p, q)) => {
                if current.last() != Some(&p) {
                    if current.len() > 1 {
                        out.0.push(LineString::new(std::mem::take(&mut current)));
                    }
                    current.clear();
                    current.push(p);
                }
                if p != q || current.len() == 1 {
                    current.push(q);
                }
                if q != w[1] {
                    // Left the box.
                    if current.len() > 1 {
                        out.0.push(LineString::new(std::mem::take(&mut current)));
                    }
                    current.clear();
                }
            }
            None => {
                if current.len() > 1 {
                    out.0.push(LineString::new(std::mem::take(&mut current)));
                }
                current.clear();
            }
        }
    }
    if current.len() > 1 {
        out.0.push(LineString::new(current));
    }
}

/// Clip a multi-linestring to `r`.
pub fn clip_multilinestring(mls: &MultiLineString, r: &Rect) -> MultiLineString {
    let mut out = MultiLineString::new(Vec::new());
    for ls in &mls.0 {
        clip_linestring(&ls.0, r, &mut out);
    }
    out
}

/// Sutherland–Hodgman clip of a closed ring against `r`. The result is
/// closed, or empty when nothing of the ring is inside.
pub fn clip_ring(ring: &[Coord], r: &Rect) -> Vec<Coord> {
    let (minx, miny, maxx, maxy) = (r.min().x, r.min().y, r.max().x, r.max().y);
    if ring.iter().all(|&c| inside(r, c)) {
        return ring.to_vec();
    }
    let mut pts: Vec<Coord> = ring.to_vec();
    if pts.first() == pts.last() {
        pts.pop();
    }
    // (inside test, intersection) per edge of the box.
    for edge in 0..4 {
        if pts.is_empty() {
            break;
        }
        let is_in = |c: Coord| match edge {
            0 => c.x >= minx,
            1 => c.x <= maxx,
            2 => c.y >= miny,
            _ => c.y <= maxy,
        };
        let cross = |a: Coord, b: Coord| -> Coord {
            match edge {
                0 | 1 => {
                    let x = if edge == 0 { minx } else { maxx };
                    let t = (x - a.x) / (b.x - a.x);
                    Coord {
                        x,
                        y: a.y + t * (b.y - a.y),
                    }
                }
                _ => {
                    let y = if edge == 2 { miny } else { maxy };
                    let t = (y - a.y) / (b.y - a.y);
                    Coord {
                        x: a.x + t * (b.x - a.x),
                        y,
                    }
                }
            }
        };
        let input = std::mem::take(&mut pts);
        let mut prev = *input.last().unwrap();
        for &cur in &input {
            match (is_in(prev), is_in(cur)) {
                (true, true) => pts.push(cur),
                (true, false) => pts.push(cross(prev, cur)),
                (false, true) => {
                    pts.push(cross(prev, cur));
                    pts.push(cur);
                }
                (false, false) => {}
            }
            prev = cur;
        }
    }
    pts.dedup();
    if pts.len() < 3 {
        return Vec::new();
    }
    pts.push(pts[0]);
    pts
}

/// Clip a multipolygon to `r`, dropping parts that fall outside.
pub fn clip_multipolygon(mp: &MultiPolygon, r: &Rect) -> MultiPolygon {
    let mut out = Vec::with_capacity(mp.0.len());
    for p in &mp.0 {
        let Some(b) = geo::BoundingRect::bounding_rect(p.exterior()) else {
            continue;
        };
        if b.max().x < r.min().x || b.min().x > r.max().x || b.max().y < r.min().y || b.min().y > r.max().y {
            continue;
        }
        let outer = clip_ring(&p.exterior().0, r);
        if outer.len() < 4 {
            continue;
        }
        let inners = p
            .interiors()
            .iter()
            .map(|i| clip_ring(&i.0, r))
            .filter(|i| i.len() >= 4)
            .map(LineString::new)
            .collect();
        out.push(Polygon::new(LineString::new(outer), inners));
    }
    MultiPolygon::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(x: f64, y: f64) -> Coord {
        Coord { x, y }
    }
    fn unit() -> Rect {
        Rect::new(c(0.0, 0.0), c(10.0, 10.0))
    }

    #[test]
    fn line_through_box_is_cut() {
        let mut out = MultiLineString::new(vec![]);
        clip_linestring(&[c(-5.0, 5.0), c(15.0, 5.0)], &unit(), &mut out);
        assert_eq!(out.0.len(), 1);
        assert_eq!(out.0[0].0, vec![c(0.0, 5.0), c(10.0, 5.0)]);
    }

    #[test]
    fn line_leaving_and_reentering_splits() {
        let mut out = MultiLineString::new(vec![]);
        clip_linestring(
            &[c(1.0, 1.0), c(1.0, 20.0), c(5.0, 20.0), c(5.0, 1.0)],
            &unit(),
            &mut out,
        );
        assert_eq!(out.0.len(), 2);
        assert_eq!(out.0[0].0, vec![c(1.0, 1.0), c(1.0, 10.0)]);
        assert_eq!(out.0[1].0, vec![c(5.0, 10.0), c(5.0, 1.0)]);
    }

    #[test]
    fn line_outside_is_dropped() {
        let mut out = MultiLineString::new(vec![]);
        clip_linestring(&[c(-5.0, -5.0), c(-1.0, 20.0)], &unit(), &mut out);
        assert!(out.0.is_empty());
    }

    #[test]
    fn polygon_is_clipped() {
        let sq = Polygon::new(
            LineString::new(vec![
                c(-5.0, -5.0),
                c(5.0, -5.0),
                c(5.0, 5.0),
                c(-5.0, 5.0),
                c(-5.0, -5.0),
            ]),
            vec![],
        );
        let out = clip_multipolygon(&MultiPolygon::new(vec![sq]), &unit());
        assert_eq!(out.0.len(), 1);
        let area = geo::Area::unsigned_area(&out);
        assert!((area - 25.0).abs() < 1e-9);
    }

    #[test]
    fn polygon_outside_is_dropped() {
        let sq = Polygon::new(
            LineString::new(vec![c(20.0, 20.0), c(30.0, 20.0), c(30.0, 30.0), c(20.0, 20.0)]),
            vec![],
        );
        assert!(clip_multipolygon(&MultiPolygon::new(vec![sq]), &unit()).0.is_empty());
    }
}
