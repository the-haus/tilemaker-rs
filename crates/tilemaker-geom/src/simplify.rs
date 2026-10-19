//! Douglas–Peucker and Visvalingam–Whyatt simplification with tilemaker's
//! thresholds and minimum point counts.

use geo::Simplify;

use crate::{LineString, MultiPolygon, Polygon};

/// Simplification algorithm of a layer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SimplifyAlgorithm {
    /// Douglas–Peucker (the default).
    #[default]
    DouglasPeucker,
    /// Visvalingam–Whyatt.
    Visvalingam,
    /// Buildings: tilemaker squares small buildings; here it falls back to
    /// Douglas–Peucker, which keeps their outlines.
    Buildings,
}

/// Simplify a linestring; `max_distance` is in the geometry's units.
pub fn simplify_linestring(ls: &LineString, max_distance: f64, algo: SimplifyAlgorithm) -> LineString {
    match algo {
        SimplifyAlgorithm::Visvalingam => {
            let closed = ls.is_closed();
            vw(ls, max_distance, if closed { 3 } else { 2 })
        }
        _ => ls.simplify(max_distance),
    }
}

fn vw(ls: &LineString, max_distance: f64, retain: usize) -> LineString {
    // tilemaker removes a point while twice its (effective) triangle area
    // is at most 8·d².
    LineString::new(visvalingam(&ls.0, max_distance * max_distance * 4.0 * 2.0, retain))
}

/// Twice the area of the triangle a, b, c.
#[inline]
fn double_area(a: crate::Coord, b: crate::Coord, c: crate::Coord) -> f64 {
    ((b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)).abs()
}

/// Visvalingam–Whyatt as tilemaker implements it (`visvalingam.cpp`):
/// repeatedly drop the point whose triangle with its neighbours is the
/// smallest, until that (doubled) area exceeds `threshold` or only `retain`
/// points are left. A neighbour's area never drops below that of the point
/// just removed (the "effective area"), so removals cascade.
pub fn visvalingam(pts: &[crate::Coord], threshold: f64, retain: usize) -> Vec<crate::Coord> {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    let n = pts.len();
    if n <= retain.max(2) {
        return pts.to_vec();
    }
    #[derive(PartialEq, PartialOrd)]
    struct Key(f64);
    impl Eq for Key {}
    impl Ord for Key {
        fn cmp(&self, o: &Self) -> std::cmp::Ordering {
            self.0.total_cmp(&o.0)
        }
    }
    let mut area = vec![f64::INFINITY; n];
    let mut prev: Vec<usize> = (0..n).map(|i| i.wrapping_sub(1)).collect();
    let mut next: Vec<usize> = (1..=n).collect();
    let mut removed = vec![false; n];
    let mut heap = BinaryHeap::with_capacity(n);
    for i in 1..n - 1 {
        area[i] = double_area(pts[i - 1], pts[i], pts[i + 1]);
        heap.push(Reverse((Key(area[i]), i)));
    }
    let mut left = n;
    while let Some(Reverse((Key(a), i))) = heap.pop() {
        if removed[i] || a != area[i] {
            continue; // a stale entry
        }
        if a > threshold || left <= retain {
            break;
        }
        removed[i] = true;
        left -= 1;
        let (p, q) = (prev[i], next[i]);
        next[p] = q;
        prev[q] = p;
        if p > 0 {
            let na = double_area(pts[prev[p]], pts[p], pts[q]).max(a);
            area[p] = na;
            heap.push(Reverse((Key(na), p)));
        }
        if q < n - 1 {
            let na = double_area(pts[p], pts[q], pts[next[q]]).max(a);
            area[q] = na;
            heap.push(Reverse((Key(na), q)));
        }
    }
    (0..n).filter(|&i| !removed[i]).map(|i| pts[i]).collect()
}

fn simplify_ring(ring: &LineString, max_distance: f64, algo: SimplifyAlgorithm) -> Option<LineString> {
    let out = match algo {
        SimplifyAlgorithm::Visvalingam => vw(ring, max_distance, 4),
        _ => ring.simplify(max_distance),
    };
    (out.0.len() >= 4).then_some(out)
}

/// Simplify every ring of a multipolygon, dropping rings that collapse.
pub fn simplify_multipolygon(mp: &MultiPolygon, max_distance: f64, algo: SimplifyAlgorithm) -> MultiPolygon {
    let polys =
        mp.0.iter()
            .filter_map(|p| {
                let outer = simplify_ring(p.exterior(), max_distance, algo)?;
                let inners = p
                    .interiors()
                    .iter()
                    .filter_map(|r| simplify_ring(r, max_distance, algo))
                    .collect();
                Some(Polygon::new(outer, inners))
            })
            .collect();
    MultiPolygon::new(polys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Coord;

    fn wiggly() -> LineString {
        LineString::new(
            (0..100)
                .map(|i| Coord {
                    x: i as f64,
                    y: if i % 2 == 0 { 0.0 } else { 0.01 },
                })
                .collect(),
        )
    }

    #[test]
    fn dp_removes_wiggles() {
        let s = simplify_linestring(&wiggly(), 0.1, SimplifyAlgorithm::DouglasPeucker);
        assert_eq!(s.0.len(), 2);
    }

    #[test]
    fn vw_removes_wiggles_but_keeps_endpoints() {
        let s = simplify_linestring(&wiggly(), 0.1, SimplifyAlgorithm::Visvalingam);
        assert!(s.0.len() < 10);
        assert_eq!(s.0.first(), wiggly().0.first());
        assert_eq!(s.0.last(), wiggly().0.last());
    }

    #[test]
    fn effective_areas_cascade() {
        // A gentle arc: each point alone is small, and removing one makes
        // its neighbours' triangles larger — the effective area keeps them
        // removable, so the arc collapses to its ends.
        let arc: Vec<Coord> = (0..=20)
            .map(|i| {
                let t = i as f64 / 20.0 * std::f64::consts::PI;
                Coord {
                    x: t.cos() * 10.0,
                    y: t.sin() * 0.2,
                }
            })
            .collect();
        // The last triangle (ends and apex) has doubled area 20 × 0.2 = 4.
        let out = visvalingam(&arc, 2.0, 2);
        assert_eq!(out.len(), 3);
        assert_eq!((out[0], out[2]), (arc[0], arc[20]));
        let out = visvalingam(&arc, 5.0, 2);
        assert_eq!(out, vec![arc[0], arc[20]]);
        // Below every triangle's area nothing goes.
        assert_eq!(visvalingam(&arc, 0.0, 2).len(), 21);
        // retain is respected.
        assert_eq!(visvalingam(&arc, f64::INFINITY, 4).len(), 4);
    }

    #[test]
    fn tiny_rings_disappear() {
        let ring = LineString::new(vec![
            Coord { x: 0.0, y: 0.0 },
            Coord { x: 0.001, y: 0.0 },
            Coord { x: 0.001, y: 0.001 },
            Coord { x: 0.0, y: 0.0 },
        ]);
        let mp = MultiPolygon::new(vec![Polygon::new(ring, vec![])]);
        assert!(simplify_multipolygon(&mp, 1.0, SimplifyAlgorithm::DouglasPeucker)
            .0
            .is_empty());
    }
}
