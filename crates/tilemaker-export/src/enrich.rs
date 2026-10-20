//! Feature post-processing: clipping, simplification and coordinate
//! precision ([`Transform`]), and values computed from the geometry —
//! area, length, centroid, label point, bounding box ([`Enrich`]).
//!
//! Geometries are in plain degrees (lon, lat). Areas and lengths are on
//! the sphere (radius 6,371 km), in m² and m.

use geo::{Centroid, Simplify};
use tilemaker_geom::clip::{clip_linestring, clip_ring};
use tilemaker_geom::coords::{lat2latp, latp2lat};
use tilemaker_geom::{Coord, LineString, MultiLineString, MultiPolygon, Polygon, Rect};

use crate::record::{Geometry, GeometryType};

const RADIUS: f64 = 6_371_000.0;

/// Changes made to every emitted geometry, in this order: clip, simplify,
/// round.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Transform {
    /// Clip to `[west, south, east, north]` (features outside are dropped;
    /// a line leaving and re-entering becomes a multi-linestring).
    pub clip: Option<[f64; 4]>,
    /// Douglas–Peucker tolerance in degrees. Rings that collapse are
    /// dropped (a polygon with its exterior).
    pub simplify: Option<f64>,
    /// Round coordinates to this many decimals (repeated points removed).
    pub precision: Option<u8>,
}

/// Values to compute for each feature.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Enrich {
    /// Area in m² (0 for points and lines).
    pub area: bool,
    /// Length in m: of lines, or the perimeter of polygons (0 for points).
    pub length: bool,
    /// Centroid `[lon, lat]` (area- or length-weighted).
    pub centroid: bool,
    /// A point on the feature for a label `[lon, lat]`: polygons' pole of
    /// inaccessibility (of the largest part), lines' midpoint.
    pub label_point: bool,
    /// Bounding box `[west, south, east, north]`.
    pub bbox: bool,
}

impl Enrich {
    /// Whether anything is computed.
    pub fn any(&self) -> bool {
        self.area || self.length || self.centroid || self.label_point || self.bbox
    }
}

/// A relation a feature's element is a member of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parent {
    /// The relation's id.
    pub id: i64,
    /// The element's role in it.
    pub role: String,
    /// The relation's tags.
    pub tags: std::sync::Arc<[(String, String)]>,
}

/// Values computed for a feature.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Extras {
    /// Area in m².
    pub area: Option<f64>,
    /// Length in m.
    pub length: Option<f64>,
    /// Centroid.
    pub centroid: Option<[f64; 2]>,
    /// Label point.
    pub label_point: Option<[f64; 2]>,
    /// Bounding box.
    pub bbox: Option<[f64; 4]>,
    /// Relations the element is a member of (with `member_of`).
    pub member_of: Option<Vec<Parent>>,
}

impl Extras {
    /// Whether nothing is set.
    pub fn is_empty(&self) -> bool {
        self.area.is_none()
            && self.length.is_none()
            && self.centroid.is_none()
            && self.label_point.is_none()
            && self.bbox.is_none()
            && self.member_of.is_none()
    }
}

fn coords(c: &[f64]) -> Vec<Coord> {
    c.chunks_exact(2).map(|p| Coord { x: p[0], y: p[1] }).collect()
}

/// Point ranges of each part (line or ring).
fn part_ranges(g: &Geometry) -> Vec<(usize, usize)> {
    let mut start = 0;
    g.parts
        .iter()
        .map(|&end| {
            let r = (start, end as usize);
            start = end as usize;
            r
        })
        .collect()
}

/// Rings of each polygon, as coordinates.
fn polygon_rings(g: &Geometry) -> Vec<Vec<Vec<Coord>>> {
    let ranges = part_ranges(g);
    let mut first = 0;
    g.polygons
        .iter()
        .map(|&end| {
            let rings = ranges[first..end as usize]
                .iter()
                .map(|&(s, e)| coords(&g.coords[2 * s..2 * e]))
                .collect();
            first = end as usize;
            rings
        })
        .collect()
}

fn lines(g: &Geometry) -> Vec<Vec<Coord>> {
    match g.kind {
        GeometryType::LineString => vec![coords(&g.coords)],
        _ => part_ranges(g)
            .into_iter()
            .map(|(s, e)| coords(&g.coords[2 * s..2 * e]))
            .collect(),
    }
}

fn from_lines(lines: Vec<Vec<Coord>>) -> Option<Geometry> {
    let mut g = Geometry {
        kind: if lines.len() == 1 {
            GeometryType::LineString
        } else {
            GeometryType::MultiLineString
        },
        coords: Vec::new(),
        parts: Vec::new(),
        polygons: Vec::new(),
    };
    if lines.is_empty() {
        return None;
    }
    for l in &lines {
        g.coords.extend(l.iter().flat_map(|c| [c.x, c.y]));
        if g.kind == GeometryType::MultiLineString {
            g.parts.push((g.coords.len() / 2) as u32);
        }
    }
    Some(g)
}

fn from_polygons(polys: Vec<Vec<Vec<Coord>>>) -> Option<Geometry> {
    if polys.is_empty() {
        return None;
    }
    let mut g = Geometry {
        kind: GeometryType::MultiPolygon,
        coords: Vec::new(),
        parts: Vec::new(),
        polygons: Vec::new(),
    };
    for p in &polys {
        for r in p {
            g.coords.extend(r.iter().flat_map(|c| [c.x, c.y]));
            g.parts.push((g.coords.len() / 2) as u32);
        }
        g.polygons.push(g.parts.len() as u32);
    }
    Some(g)
}

impl Transform {
    /// Whether this changes nothing.
    pub fn is_identity(&self) -> bool {
        self.clip.is_none() && self.simplify.is_none() && self.precision.is_none()
    }

    fn round(&self, v: f64) -> f64 {
        match self.precision {
            Some(p) => {
                let f = 10f64.powi(p as i32);
                (v * f).round() / f
            }
            None => v,
        }
    }

    /// A line or ring after simplification and rounding (repeated points
    /// removed); `None` when fewer than `min` points are left.
    fn finish(&self, pts: Vec<Coord>, closed: bool, min: usize) -> Option<Vec<Coord>> {
        let mut pts = match self.simplify {
            Some(t) if t > 0.0 => LineString::new(pts).simplify(t).0,
            _ => pts,
        };
        if self.precision.is_some() {
            for c in &mut pts {
                *c = Coord {
                    x: self.round(c.x),
                    y: self.round(c.y),
                };
            }
            pts.dedup();
            if closed && pts.len() > 1 && pts[0] != pts[pts.len() - 1] {
                pts.push(pts[0]);
            }
        }
        (pts.len() >= min).then_some(pts)
    }

    /// Apply to a geometry; `None` when nothing of it is left.
    pub fn apply(&self, g: Geometry) -> Option<Geometry> {
        if self.is_identity() {
            return Some(g);
        }
        let rect = self
            .clip
            .map(|b| Rect::new(Coord { x: b[0], y: b[1] }, Coord { x: b[2], y: b[3] }));
        match g.kind {
            GeometryType::Point => {
                let (x, y) = (g.coords[0], g.coords[1]);
                if let Some(b) = self.clip {
                    if x < b[0] || x > b[2] || y < b[1] || y > b[3] {
                        return None;
                    }
                }
                Some(Geometry {
                    coords: vec![self.round(x), self.round(y)],
                    ..g
                })
            }
            GeometryType::LineString | GeometryType::MultiLineString => {
                let mut pieces = lines(&g);
                if let Some(r) = &rect {
                    let mut out = MultiLineString::new(Vec::new());
                    for l in &pieces {
                        clip_linestring(l, r, &mut out);
                    }
                    pieces = out.0.into_iter().map(|l| l.0).collect();
                }
                let pieces: Vec<Vec<Coord>> = pieces.into_iter().filter_map(|l| self.finish(l, false, 2)).collect();
                // A linestring stays one unless clipping split it.
                match (g.kind, pieces.len()) {
                    (GeometryType::MultiLineString, 1) => {
                        let mut out = from_lines(pieces)?;
                        out.kind = GeometryType::MultiLineString;
                        out.parts = vec![(out.coords.len() / 2) as u32];
                        Some(out)
                    }
                    _ => from_lines(pieces),
                }
            }
            GeometryType::MultiPolygon => {
                let mut polys = Vec::new();
                for rings in polygon_rings(&g) {
                    let mut kept = Vec::new();
                    for (i, ring) in rings.into_iter().enumerate() {
                        let ring = match &rect {
                            Some(r) => clip_ring(&ring, r),
                            None => ring,
                        };
                        match self.finish(ring, true, 4) {
                            Some(r) => kept.push(r),
                            None if i == 0 => break,
                            None => {}
                        }
                    }
                    if !kept.is_empty() {
                        polys.push(kept);
                    }
                }
                from_polygons(polys)
            }
        }
    }
}

/// Spherical area of a ring in m² (Chamberlain & Duquette), unsigned.
fn ring_area(r: &[Coord]) -> f64 {
    if r.len() < 3 {
        return 0.0;
    }
    let mut total = 0.0;
    for w in r.windows(2) {
        let (a, b) = (w[0], w[1]);
        total += (b.x - a.x).to_radians() * (2.0 + a.y.to_radians().sin() + b.y.to_radians().sin());
    }
    (total * RADIUS * RADIUS / 2.0).abs()
}

fn haversine(a: Coord, b: Coord) -> f64 {
    let (p1, p2) = (a.y.to_radians(), b.y.to_radians());
    let dp = p2 - p1;
    let dl = (b.x - a.x).to_radians();
    let h = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * RADIUS * h.sqrt().min(1.0).asin()
}

fn line_length(l: &[Coord]) -> f64 {
    l.windows(2).map(|w| haversine(w[0], w[1])).sum()
}

/// The point halfway along the lines (by length).
fn midpoint(lines: &[Vec<Coord>]) -> Option<[f64; 2]> {
    let total: f64 = lines.iter().map(|l| line_length(l)).sum();
    let mut left = total / 2.0;
    for l in lines {
        for w in l.windows(2) {
            let d = haversine(w[0], w[1]);
            if d >= left && d > 0.0 {
                let t = left / d;
                return Some([w[0].x + t * (w[1].x - w[0].x), w[0].y + t * (w[1].y - w[0].y)]);
            }
            left -= d;
        }
    }
    lines.iter().find_map(|l| l.first()).map(|c| [c.x, c.y])
}

/// The pole of inaccessibility of the largest polygon, found in Mercator
/// space to a precision of a thousandth of its size.
fn label_point(polys: &[Vec<Vec<Coord>>]) -> Option<[f64; 2]> {
    let biggest = polys
        .iter()
        .max_by(|a, b| ring_area(&a[0]).total_cmp(&ring_area(&b[0])))?;
    let project = |r: &Vec<Coord>| {
        LineString::new(
            r.iter()
                .map(|c| Coord {
                    x: c.x,
                    y: lat2latp(c.y),
                })
                .collect(),
        )
    };
    let p = Polygon::new(project(&biggest[0]), biggest[1..].iter().map(project).collect());
    let b = geo::BoundingRect::bounding_rect(&p)?;
    let tolerance = (b.width().max(b.height()) / 1000.0).max(1e-7);
    let pt = polylabel::polylabel(&p, &tolerance).ok().or_else(|| p.centroid())?;
    Some([pt.x(), latp2lat(pt.y())])
}

impl Enrich {
    /// Compute the requested values for a geometry. Points come rounded to
    /// 1e-7 degrees, like OSM's.
    pub fn compute(&self, g: &Geometry) -> Extras {
        let mut e = self.compute_raw(g);
        let round = |p: [f64; 2]| p.map(|v| (v * 1e7).round() / 1e7);
        e.centroid = e.centroid.map(round);
        e.label_point = e.label_point.map(round);
        e
    }

    fn compute_raw(&self, g: &Geometry) -> Extras {
        let mut e = Extras::default();
        if self.bbox {
            e.bbox = Some(g.bbox());
        }
        match g.kind {
            GeometryType::Point => {
                let p = [g.coords[0], g.coords[1]];
                e.area = self.area.then_some(0.0);
                e.length = self.length.then_some(0.0);
                e.centroid = self.centroid.then_some(p);
                e.label_point = self.label_point.then_some(p);
            }
            GeometryType::LineString | GeometryType::MultiLineString => {
                let ls = lines(g);
                e.area = self.area.then_some(0.0);
                if self.length {
                    e.length = Some(ls.iter().map(|l| line_length(l)).sum());
                }
                if self.centroid {
                    let mls = MultiLineString::new(ls.iter().map(|l| LineString::new(l.clone())).collect());
                    e.centroid = mls.centroid().map(|p| [p.x(), p.y()]);
                }
                if self.label_point {
                    e.label_point = midpoint(&ls);
                }
            }
            GeometryType::MultiPolygon => {
                let polys = polygon_rings(g);
                if self.area {
                    e.area = Some(
                        polys
                            .iter()
                            .map(|p| (ring_area(&p[0]) - p[1..].iter().map(|h| ring_area(h)).sum::<f64>()).max(0.0))
                            .sum(),
                    );
                }
                if self.length {
                    e.length = Some(polys.iter().flatten().map(|r| line_length(r)).sum());
                }
                if self.centroid {
                    let mp = MultiPolygon::new(
                        polys
                            .iter()
                            .map(|p| {
                                Polygon::new(
                                    LineString::new(p[0].clone()),
                                    p[1..].iter().map(|h| LineString::new(h.clone())).collect(),
                                )
                            })
                            .collect(),
                    );
                    e.centroid = mp.centroid().map(|p| [p.x(), p.y()]);
                }
                if self.label_point {
                    e.label_point = label_point(&polys);
                }
            }
        }
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<f64> {
        vec![x, y, x + s, y, x + s, y + s, x, y + s, x, y]
    }

    fn polygon(rings: &[Vec<f64>]) -> Geometry {
        let mut g = Geometry {
            kind: GeometryType::MultiPolygon,
            coords: Vec::new(),
            parts: Vec::new(),
            polygons: Vec::new(),
        };
        for r in rings {
            g.coords.extend(r);
            g.parts.push((g.coords.len() / 2) as u32);
        }
        g.polygons.push(rings.len() as u32);
        g
    }

    fn line(c: &[f64]) -> Geometry {
        Geometry {
            kind: GeometryType::LineString,
            coords: c.to_vec(),
            parts: Vec::new(),
            polygons: Vec::new(),
        }
    }

    #[test]
    fn measures() {
        let all = Enrich {
            area: true,
            length: true,
            centroid: true,
            label_point: true,
            bbox: true,
        };
        // 0.01° square at the equator: ~1113 m sides.
        let e = all.compute(&polygon(&[square(0.0, 0.0, 0.01)]));
        let side = 2.0 * std::f64::consts::PI * RADIUS / 36_000.0;
        assert!(
            (e.area.unwrap() - side * side).abs() / (side * side) < 1e-3,
            "{:?}",
            e.area
        );
        assert!((e.length.unwrap() - 4.0 * side).abs() < 1.0, "{:?}", e.length);
        let c = e.centroid.unwrap();
        assert!((c[0] - 0.005).abs() < 1e-9 && (c[1] - 0.005).abs() < 1e-6);
        let l = e.label_point.unwrap();
        assert!((l[0] - 0.005).abs() < 1e-4 && (l[1] - 0.005).abs() < 1e-4, "{l:?}");
        assert_eq!(e.bbox, Some([0.0, 0.0, 0.01, 0.01]));
        // A hole takes its area away; the label point avoids it.
        let holed = all.compute(&polygon(&[square(0.0, 0.0, 0.01), square(0.002, 0.002, 0.006)]));
        assert!(holed.area.unwrap() < e.area.unwrap() * 0.65);
        let l = holed.label_point.unwrap();
        assert!(!(l[0] > 0.002 && l[0] < 0.008 && l[1] > 0.002 && l[1] < 0.008), "{l:?}");
        // A line: length and midpoint.
        let e = all.compute(&line(&[0.0, 0.0, 0.0, 1.0, 0.0, 3.0]));
        assert!((e.length.unwrap() - 3.0 * side * 100.0).abs() < 10.0);
        let m = e.label_point.unwrap();
        assert!((m[1] - 1.5).abs() < 1e-9, "{m:?}");
        assert_eq!(e.area, Some(0.0));
    }

    #[test]
    fn transforms() {
        let clip = Transform {
            clip: Some([0.0, 0.0, 1.0, 1.0]),
            ..Default::default()
        };
        // A line through the box and out again, twice.
        let g = clip
            .apply(line(&[-1.0, 0.5, 0.5, 0.5, 0.5, 2.0, 0.7, 2.0, 0.7, 0.5, 2.0, 0.5]))
            .unwrap();
        assert_eq!(g.kind, GeometryType::MultiLineString);
        assert_eq!(g.parts.len(), 2);
        // A polygon half outside.
        let g = clip.apply(polygon(&[square(0.5, 0.5, 1.0)])).unwrap();
        assert_eq!(g.bbox(), [0.5, 0.5, 1.0, 1.0]);
        assert!(clip.apply(polygon(&[square(5.0, 5.0, 1.0)])).is_none());
        // Simplification drops a nearly straight vertex; precision rounds.
        let t = Transform {
            simplify: Some(0.001),
            precision: Some(2),
            ..Default::default()
        };
        let g = t.apply(line(&[0.0, 0.0, 0.5, 0.0001, 1.0, 0.0])).unwrap();
        assert_eq!(g.coords, vec![0.0, 0.0, 1.0, 0.0]);
        let g = t.apply(line(&[0.123456, 1.0, 0.124, 1.0])).unwrap_or_else(|| line(&[]));
        assert!(g.coords.is_empty(), "collapses to one point: dropped");
        // A ring that collapses drops its polygon.
        assert!(t.apply(polygon(&[square(0.0, 0.0, 0.001)])).is_none());
    }
}
