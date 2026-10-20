//! Point-in-polygon lookups over many (multi)polygons: which areas contain
//! a point, smallest first — reverse geocoding a place to its country and
//! region, for instance.
//!
//! Polygon parts go into an R-tree by bounding box; each ring keeps its
//! points at 1e-7 degrees (8 bytes each) and a banded index of its edges,
//! so a test against a million-point coastline costs about a thousand
//! exact integer edge checks. A point on a boundary counts as inside.

use rstar::primitives::{GeomWithData, Rectangle};
use rstar::{RTree, AABB};
use tilemaker_geom::Coord;

use crate::record::{Geometry, GeometryType};

const RADIUS: f64 = 6_371_000.0;

/// A ring for point-in-ring tests: closed, at 1e-7 degrees, with the
/// edges crossing each horizontal band listed (by first point).
struct Ring {
    pts: Vec<[i32; 2]>,
    min_y: i64,
    band_h: i64,
    /// Where each band's edges start in `edges` (one more than bands).
    starts: Vec<u32>,
    edges: Vec<u32>,
}

impl Ring {
    fn new(coords: &[Coord]) -> Self {
        let mut pts: Vec<[i32; 2]> = coords
            .iter()
            .map(|c| [(c.x * 1e7).round() as i32, (c.y * 1e7).round() as i32])
            .collect();
        pts.dedup();
        if pts.len() > 1 && pts[0] != pts[pts.len() - 1] {
            pts.push(pts[0]);
        }
        let n_edges = pts.len().saturating_sub(1);
        let (min_y, max_y) = pts.iter().fold((i64::MAX, i64::MIN), |(lo, hi), p| {
            (lo.min(p[1] as i64), hi.max(p[1] as i64))
        });
        let bands = if n_edges < 64 {
            1
        } else {
            (n_edges as f64).sqrt() as usize
        };
        let band_h = ((max_y - min_y) / bands as i64 + 1).max(1);
        let band = |y: i32| (((y as i64 - min_y) / band_h) as usize).min(bands - 1);
        // Count, then fill (CSR).
        let mut starts = vec![0u32; bands + 1];
        for w in pts.windows(2) {
            let (lo, hi) = (band(w[0][1].min(w[1][1])), band(w[0][1].max(w[1][1])));
            for b in lo..=hi {
                starts[b + 1] += 1;
            }
        }
        for b in 0..bands {
            starts[b + 1] += starts[b];
        }
        let mut fill = starts.clone();
        let mut edges = vec![0u32; *starts.last().unwrap_or(&0) as usize];
        for (i, w) in pts.windows(2).enumerate() {
            let (lo, hi) = (band(w[0][1].min(w[1][1])), band(w[0][1].max(w[1][1])));
            for b in lo..=hi {
                edges[fill[b] as usize] = i as u32;
                fill[b] += 1;
            }
        }
        Ring {
            pts,
            min_y,
            band_h,
            starts,
            edges,
        }
    }

    /// Even–odd test at 1e-7 degrees; `None` when on the ring.
    fn contains(&self, x: i64, y: i64) -> Option<bool> {
        let bands = self.starts.len() - 1;
        if self.pts.len() < 4 || y < self.min_y {
            return Some(false);
        }
        let b = ((y - self.min_y) / self.band_h) as usize;
        if b >= bands && y > self.min_y + self.band_h * bands as i64 {
            return Some(false);
        }
        let b = b.min(bands - 1);
        let mut inside = false;
        for &e in &self.edges[self.starts[b] as usize..self.starts[b + 1] as usize] {
            let (a, c) = (self.pts[e as usize], self.pts[e as usize + 1]);
            let (ax, ay, cx, cy) = (a[0] as i64, a[1] as i64, c[0] as i64, c[1] as i64);
            if (ay > y) != (cy > y) {
                // The crossing's x against the point's, exactly:
                // sign((x − ax)(cy − ay) − (cx − ax)(y − ay)), by direction.
                let d = (x - ax) as i128 * (cy - ay) as i128 - (cx - ax) as i128 * (y - ay) as i128;
                if d == 0 {
                    return None;
                }
                if (d < 0) == (cy > ay) {
                    inside = !inside;
                }
            } else if (ay == y && cy == y && x >= ax.min(cx) && x <= ax.max(cx))
                || (ax, ay) == (x, y)
                || (cx, cy) == (x, y)
            {
                // On a horizontal edge or a vertex.
                return None;
            }
        }
        Some(inside)
    }

    fn bytes(&self) -> usize {
        self.pts.len() * 8 + self.edges.len() * 4 + self.starts.len() * 4
    }
}

/// One polygon of a feature: exterior first, then holes.
struct Part {
    feature: u32,
    rings: Vec<Ring>,
}

/// Polygons indexed for point lookups (see the module docs).
#[derive(Default)]
pub struct PolygonIndex {
    parts: Vec<Part>,
    /// Per feature: its area in m² (lookups sort by it).
    areas: Vec<f64>,
    tree: RTree<GeomWithData<Rectangle<[f64; 2]>, u32>>,
    pending: Vec<GeomWithData<Rectangle<[f64; 2]>, u32>>,
}

/// Spherical area of a ring in m², unsigned.
fn ring_area(r: &[Coord]) -> f64 {
    let mut total = 0.0;
    for w in r.windows(2) {
        total += (w[1].x - w[0].x).to_radians() * (2.0 + w[0].y.to_radians().sin() + w[1].y.to_radians().sin());
    }
    (total * RADIUS * RADIUS / 2.0).abs()
}

impl PolygonIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of features added.
    pub fn len(&self) -> usize {
        self.areas.len()
    }

    /// Whether nothing was added.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }

    /// Feature `i`'s area in m².
    pub fn area(&self, i: u32) -> f64 {
        self.areas[i as usize]
    }

    /// Add a multipolygon as flat arrays (as [`Geometry`] holds them):
    /// `coords` lon, lat pairs; `parts` each ring's exclusive end in points;
    /// `polygons` each polygon's exclusive end in rings. Returns the
    /// feature's index (in the order added).
    pub fn add(&mut self, coords: &[f64], parts: &[u32], polygons: &[u32]) -> u32 {
        let feature = self.areas.len() as u32;
        let mut area = 0.0;
        let (mut ring, mut point) = (0usize, 0usize);
        for &ring_end in polygons {
            let mut rings = Vec::new();
            let mut bbox = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
            for (k, &end) in parts[ring..ring_end as usize].iter().enumerate() {
                let end = end as usize;
                let pts: Vec<Coord> = coords[2 * point..2 * end]
                    .chunks_exact(2)
                    .map(|c| Coord { x: c[0], y: c[1] })
                    .collect();
                if k == 0 {
                    for c in &pts {
                        bbox = [bbox[0].min(c.x), bbox[1].min(c.y), bbox[2].max(c.x), bbox[3].max(c.y)];
                    }
                    area += ring_area(&pts);
                } else {
                    area -= ring_area(&pts);
                }
                rings.push(Ring::new(&pts));
                point = end;
            }
            ring = ring_end as usize;
            if rings.is_empty() || !bbox[0].is_finite() {
                continue;
            }
            let part = self.parts.len() as u32;
            self.parts.push(Part { feature, rings });
            self.pending.push(GeomWithData::new(
                Rectangle::from_corners([bbox[0], bbox[1]], [bbox[2], bbox[3]]),
                part,
            ));
        }
        self.areas.push(area.max(0.0));
        feature
    }

    /// Add a feature's geometry (only multipolygons hold area; others are
    /// added empty, so indices still follow the features).
    pub fn add_geometry(&mut self, g: &Geometry) -> u32 {
        match g.kind {
            GeometryType::MultiPolygon => self.add(&g.coords, &g.parts, &g.polygons),
            _ => self.add(&[], &[], &[]),
        }
    }

    /// Build the tree from what was added (call before lookups; adding more
    /// later needs another build).
    pub fn build(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut all: Vec<_> = self.tree.iter().cloned().collect();
        all.append(&mut self.pending);
        self.tree = RTree::bulk_load(all);
    }

    fn part_contains(&self, part: &Part, x: i64, y: i64) -> bool {
        match part.rings[0].contains(x, y) {
            None => true,
            Some(false) => false,
            Some(true) => part.rings[1..].iter().all(|h| h.contains(x, y) != Some(true)),
        }
    }

    /// The features containing `(lon, lat)`, smallest area first.
    pub fn lookup(&self, lon: f64, lat: f64) -> Vec<u32> {
        let (x, y) = ((lon * 1e7).round() as i64, (lat * 1e7).round() as i64);
        let mut out: Vec<u32> = self
            .tree
            .locate_in_envelope_intersecting(&AABB::from_point([lon, lat]))
            .map(|g| &self.parts[g.data as usize])
            .filter(|part| self.part_contains(part, x, y))
            .map(|part| part.feature)
            .collect();
        out.sort_unstable_by(|a, b| {
            self.areas[*a as usize]
                .total_cmp(&self.areas[*b as usize])
                .then(a.cmp(b))
        });
        out.dedup();
        out
    }

    /// The smallest feature containing `(lon, lat)`.
    pub fn smallest(&self, lon: f64, lat: f64) -> Option<u32> {
        let (x, y) = ((lon * 1e7).round() as i64, (lat * 1e7).round() as i64);
        self.tree
            .locate_in_envelope_intersecting(&AABB::from_point([lon, lat]))
            .map(|g| &self.parts[g.data as usize])
            .filter(|part| self.part_contains(part, x, y))
            .map(|part| part.feature)
            .min_by(|a, b| {
                self.areas[*a as usize]
                    .total_cmp(&self.areas[*b as usize])
                    .then(a.cmp(b))
            })
    }

    /// [`smallest`](Self::smallest) for many points (`lon, lat, …`), on
    /// `threads` threads (0: all cores).
    pub fn smallest_many(&self, points: &[f64], threads: usize) -> Vec<Option<u32>> {
        let n = points.len() / 2;
        let threads = crate::engine::threads(threads).min(n.div_ceil(1024)).max(1);
        let mut out = vec![None; n];
        let chunk = n.div_ceil(threads).max(1);
        std::thread::scope(|s| {
            for (k, slot) in out.chunks_mut(chunk).enumerate() {
                let pts = &points[2 * k * chunk..];
                s.spawn(move || {
                    for (i, o) in slot.iter_mut().enumerate() {
                        *o = self.smallest(pts[2 * i], pts[2 * i + 1]);
                    }
                });
            }
        });
        out
    }

    /// Approximate heap bytes.
    pub fn heap_bytes(&self) -> usize {
        self.parts.iter().flat_map(|p| &p.rings).map(Ring::bytes).sum::<usize>()
            + self.tree.size() * 48
            + self.areas.len() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<f64> {
        vec![x, y, x + s, y, x + s, y + s, x, y + s, x, y]
    }

    #[test]
    fn nested_and_holed() {
        let mut idx = PolygonIndex::new();
        // A country with a lake (hole), a region inside it, and a separate
        // island (second polygon of the country).
        let mut country = square(0.0, 0.0, 10.0);
        country.extend(square(4.0, 4.0, 1.0));
        country.extend(square(20.0, 20.0, 1.0));
        let c = idx.add(&country, &[5, 10, 15], &[2, 3]);
        let r = idx.add(&square(1.0, 1.0, 2.0), &[5], &[1]);
        let line = idx.add_geometry(&Geometry {
            kind: GeometryType::LineString,
            coords: vec![0.0, 0.0, 1.0, 1.0],
            parts: vec![],
            polygons: vec![],
        });
        idx.build();
        assert_eq!(idx.len(), 3);
        assert_eq!(idx.lookup(1.5, 1.5), vec![r, c]);
        assert_eq!(idx.smallest(1.5, 1.5), Some(r));
        assert_eq!(idx.lookup(6.0, 6.0), vec![c]);
        assert_eq!(idx.lookup(4.5, 4.5), Vec::<u32>::new(), "in the lake");
        assert_eq!(idx.lookup(20.5, 20.5), vec![c], "on the island");
        assert_eq!(idx.lookup(10.0, 5.0), vec![c], "on the border");
        assert_eq!(idx.smallest(50.0, 50.0), None);
        assert!(idx.area(r) < idx.area(c));
        assert_eq!(idx.area(line), 0.0);
        let pts: Vec<f64> = (0..5000).flat_map(|i| [1.5 + (i % 3) as f64 * 3.0, 1.5]).collect();
        let many = idx.smallest_many(&pts, 4);
        assert_eq!(many.len(), 5000);
        assert_eq!(&many[..3], &[Some(r), Some(c), Some(c)]);
        assert!(idx.heap_bytes() > 0);
    }

    #[test]
    fn rings_agree_with_a_plain_even_odd_test() {
        // A star with 400 points (banded), against a brute-force test.
        let pts: Vec<Coord> = (0..=400)
            .map(|i| {
                let t = (i % 400) as f64 / 400.0 * std::f64::consts::TAU;
                let r = if i % 2 == 0 { 1.0 } else { 0.4 };
                Coord {
                    x: r * t.cos(),
                    y: r * t.sin(),
                }
            })
            .collect();
        let ring = Ring::new(&pts);
        let brute = |x: f64, y: f64| {
            let mut inside = false;
            for w in pts.windows(2) {
                let (a, b) = (w[0], w[1]);
                if (a.y > y) != (b.y > y) && x < (b.x - a.x) * (y - a.y) / (b.y - a.y) + a.x {
                    inside = !inside;
                }
            }
            inside
        };
        let mut seed = 3u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64 * 2.4 - 1.2
        };
        for _ in 0..20_000 {
            let (x, y) = (rnd(), rnd());
            let got = ring.contains((x * 1e7).round() as i64, (y * 1e7).round() as i64);
            if let Some(g) = got {
                assert_eq!(g, brute(x, y), "{x} {y}");
            }
        }
        // Vertices and edges are on the ring.
        assert_eq!(ring.contains(10_000_000, 0), None);
        assert_eq!(ring.contains(0, 0), Some(true));
        assert_eq!(ring.contains(20_000_000, 0), Some(false));
    }
}
