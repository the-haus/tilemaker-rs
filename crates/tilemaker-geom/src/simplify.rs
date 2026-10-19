//! Douglas–Peucker and Visvalingam–Whyatt simplification with tilemaker's
//! thresholds and minimum point counts.

use geo::{Simplify, SimplifyVw};

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
    if ls.0.len() <= retain {
        return ls.clone();
    }
    // tilemaker removes a point when twice its triangle's area is under
    // 8·d², i.e. when the area is under 4·d².
    let out = ls.simplify_vw(max_distance * max_distance * 4.0);
    if out.0.len() < retain {
        ls.clone()
    } else {
        out
    }
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
