//! Winding, validity repair and unions.

use geo::orient::{Direction, Orient};
use geo::{BooleanOps, BoundingRect, Validation};

use crate::{MultiPolygon, Polygon};

/// Orient exteriors counter-clockwise and holes clockwise (in lon/latp,
/// y up), which becomes the MVT winding once y is flipped into tile space.
pub fn orient_multipolygon(mp: &mut MultiPolygon) {
    *mp = mp.orient(Direction::Default);
}

/// Whether a multipolygon is valid OGC geometry.
pub fn is_valid(mp: &MultiPolygon) -> bool {
    mp.is_valid()
}

/// Repair an invalid multipolygon polygon by polygon, keeping a polygon as
/// it is when its repair would lose more than half of its area (tilemaker's
/// area-preserving policy). Returns whether everything ended up valid.
pub fn repair_multipolygon(mp: &mut MultiPolygon) -> bool {
    if mp.is_valid() {
        return true;
    }
    let mut all_valid = true;
    let mut out: Vec<Polygon> = Vec::with_capacity(mp.0.len());
    for p in mp.0.drain(..) {
        if p.is_valid() {
            out.push(p);
            continue;
        }
        let before = geo::Area::unsigned_area(&p);
        // A union with nothing resolves self-intersections (non-zero fill).
        let fixed = MultiPolygon::new(vec![p.clone()]).union(&MultiPolygon::new(vec![]));
        if geo::Area::unsigned_area(&fixed) >= 0.5 * before && fixed.is_valid() {
            out.extend(fixed.0);
        } else {
            all_valid = false;
            out.push(p);
        }
    }
    *mp = MultiPolygon::new(out);
    orient_multipolygon(mp);
    all_valid
}

/// Union many multipolygons, only running the (expensive) boolean union
/// among groups whose bounding boxes overlap.
pub fn union_many(parts: Vec<MultiPolygon>) -> MultiPolygon {
    let parts: Vec<MultiPolygon> = parts.into_iter().filter(|m| !m.0.is_empty()).collect();
    if parts.len() < 2 {
        return parts.into_iter().next().unwrap_or_else(|| MultiPolygon::new(vec![]));
    }
    let boxes: Vec<_> = parts.iter().map(|m| m.bounding_rect().unwrap()).collect();
    let mut parent: Vec<usize> = (0..parts.len()).collect();
    fn find(p: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while p[r] != r {
            r = p[r];
        }
        let mut c = x;
        while p[c] != r {
            let n = p[c];
            p[c] = r;
            c = n;
        }
        r
    }
    for i in 0..parts.len() {
        for j in 0..i {
            let (a, b) = (boxes[i], boxes[j]);
            let overlap =
                a.min().x <= b.max().x && b.min().x <= a.max().x && a.min().y <= b.max().y && b.min().y <= a.max().y;
            if overlap {
                let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }
    let mut groups: std::collections::BTreeMap<usize, Vec<MultiPolygon>> = Default::default();
    for (i, m) in parts.into_iter().enumerate() {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(m);
    }
    let mut out = Vec::new();
    for (_, mut group) in groups {
        let mut acc = group.remove(0);
        for m in group {
            acc = acc.union(&m);
        }
        out.extend(acc.0);
    }
    let mut mp = MultiPolygon::new(out);
    orient_multipolygon(&mut mp);
    mp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Coord, LineString};
    use geo::Area;

    fn sq(x0: f64, y0: f64, s: f64) -> MultiPolygon {
        let c = |x, y| Coord { x, y };
        MultiPolygon::new(vec![Polygon::new(
            LineString::new(vec![
                c(x0, y0),
                c(x0 + s, y0),
                c(x0 + s, y0 + s),
                c(x0, y0 + s),
                c(x0, y0),
            ]),
            vec![],
        )])
    }

    #[test]
    fn unions_overlapping_and_keeps_disjoint() {
        let u = union_many(vec![sq(0.0, 0.0, 2.0), sq(1.0, 1.0, 2.0), sq(10.0, 10.0, 1.0)]);
        assert_eq!(u.0.len(), 2);
        assert!((u.unsigned_area() - (7.0 + 1.0)).abs() < 1e-9);
    }

    #[test]
    fn repairs_bowtie() {
        let c = |x, y| Coord { x, y };
        let bowtie = Polygon::new(
            LineString::new(vec![c(0.0, 0.0), c(2.0, 2.0), c(2.0, 0.0), c(0.0, 2.0), c(0.0, 0.0)]),
            vec![],
        );
        let mut mp = MultiPolygon::new(vec![bowtie]);
        assert!(!is_valid(&mp));
        assert!(repair_multipolygon(&mut mp));
        assert!(is_valid(&mp));
        assert!((mp.unsigned_area() - 2.0).abs() < 1e-9);
    }
}
