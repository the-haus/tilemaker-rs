//! Compact geometries for storage: fixed-point positions (8 bytes a point,
//! 1e-7 degrees, ~1 cm) in flat arrays with part offsets, instead of
//! `geo`'s nested vectors of `f64` pairs. Converted back to `geo` types
//! when they are worked on.

use crate::coords::LatpLon;
use crate::{ll_to_coord, Coord, LineString, MultiLineString, MultiPolygon, Polygon};

/// Round a coordinate to fixed point.
#[inline]
pub fn quantize(c: Coord) -> LatpLon {
    LatpLon {
        latp: (c.y * 1e7).round() as i32,
        lon: (c.x * 1e7).round() as i32,
    }
}

/// Fixed-point positions as `geo` coordinates.
pub fn to_coords(lls: &[LatpLon]) -> Vec<Coord> {
    lls.iter().map(|&ll| ll_to_coord(ll)).collect()
}

/// Several lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactLines {
    /// All points.
    pub coords: Box<[LatpLon]>,
    /// Exclusive end of each line, in points.
    pub parts: Box<[u32]>,
}

impl CompactLines {
    /// From a multi-linestring.
    pub fn from_geo(mls: &MultiLineString) -> Self {
        let mut coords = Vec::with_capacity(mls.0.iter().map(|l| l.0.len()).sum());
        let mut parts = Vec::with_capacity(mls.0.len());
        for l in &mls.0 {
            coords.extend(l.0.iter().map(|&c| quantize(c)));
            parts.push(coords.len() as u32);
        }
        Self {
            coords: coords.into(),
            parts: parts.into(),
        }
    }

    /// The lines.
    pub fn lines(&self) -> impl Iterator<Item = &[LatpLon]> + '_ {
        let mut start = 0usize;
        self.parts.iter().map(move |&end| {
            let s = &self.coords[start..end as usize];
            start = end as usize;
            s
        })
    }

    /// Number of lines.
    pub fn len(&self) -> usize {
        self.parts.len()
    }

    /// Whether there are no lines.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// As a multi-linestring.
    pub fn to_geo(&self) -> MultiLineString {
        MultiLineString::new(self.lines().map(|l| LineString::new(to_coords(l))).collect())
    }
}

/// Polygons with holes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompactPolygons {
    /// All points.
    pub coords: Box<[LatpLon]>,
    /// Exclusive end of each ring, in points.
    pub rings: Box<[u32]>,
    /// Exclusive end of each polygon, in rings (its first ring is the
    /// exterior).
    pub polygons: Box<[u32]>,
    /// Each polygon's exterior box: `[min lon, min latp, max lon, max latp]`.
    pub boxes: Box<[[i32; 4]]>,
}

impl CompactPolygons {
    /// From a multipolygon.
    pub fn from_geo(mp: &MultiPolygon) -> Self {
        let mut coords = Vec::new();
        let mut rings = Vec::new();
        let mut polygons = Vec::with_capacity(mp.0.len());
        for p in &mp.0 {
            for r in std::iter::once(p.exterior()).chain(p.interiors()) {
                coords.extend(r.0.iter().map(|&c| quantize(c)));
                rings.push(coords.len() as u32);
            }
            polygons.push(rings.len() as u32);
        }
        let mut s = Self {
            coords: coords.into(),
            rings: rings.into(),
            polygons: polygons.into(),
            boxes: Box::new([]),
        };
        s.boxes = s
            .exteriors()
            .map(|r| {
                r.iter().fold([i32::MAX, i32::MAX, i32::MIN, i32::MIN], |b, p| {
                    [b[0].min(p.lon), b[1].min(p.latp), b[2].max(p.lon), b[3].max(p.latp)]
                })
            })
            .collect();
        s
    }

    /// The rings of polygon `i` (exterior first).
    fn ring(&self, r: usize) -> &[LatpLon] {
        let start = if r == 0 { 0 } else { self.rings[r - 1] as usize };
        &self.coords[start..self.rings[r] as usize]
    }

    /// Each polygon's rings, exterior first.
    pub fn polygons(&self) -> impl Iterator<Item = Vec<&[LatpLon]>> + '_ {
        let mut first = 0usize;
        self.polygons.iter().map(move |&end| {
            let rs = (first..end as usize).map(|r| self.ring(r)).collect();
            first = end as usize;
            rs
        })
    }

    /// The exterior rings.
    pub fn exteriors(&self) -> impl Iterator<Item = &[LatpLon]> + '_ {
        let mut first = 0usize;
        self.polygons.iter().map(move |&end| {
            let r = self.ring(first);
            first = end as usize;
            r
        })
    }

    /// Number of polygons.
    pub fn len(&self) -> usize {
        self.polygons.len()
    }

    /// Whether there are no polygons.
    pub fn is_empty(&self) -> bool {
        self.polygons.is_empty()
    }

    /// As a multipolygon.
    pub fn to_geo(&self) -> MultiPolygon {
        MultiPolygon::new(
            self.polygons()
                .map(|rs| {
                    let mut it = rs.into_iter().map(|r| LineString::new(to_coords(r)));
                    let ext = it.next().unwrap_or_else(|| LineString::new(Vec::new()));
                    Polygon::new(ext, it.collect())
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        let c = |x: f64, y: f64| Coord { x, y };
        let sq =
            |x0: f64, s: f64| LineString::new(vec![c(x0, 0.0), c(x0 + s, 0.0), c(x0 + s, s), c(x0, s), c(x0, 0.0)]);
        let mp = MultiPolygon::new(vec![
            Polygon::new(sq(0.0, 4.0), vec![sq(1.0, 1.0)]),
            Polygon::new(sq(10.0, 2.0), vec![]),
        ]);
        let cp = CompactPolygons::from_geo(&mp);
        assert_eq!(cp.len(), 2);
        assert_eq!(cp.to_geo(), mp);
        assert_eq!(cp.exteriors().count(), 2);
        assert_eq!(cp.polygons().map(|p| p.len()).collect::<Vec<_>>(), vec![2, 1]);
        let mls = MultiLineString::new(vec![sq(0.0, 1.0), LineString::new(vec![c(-1.5, 2.25), c(3.0, -4.0)])]);
        let cl = CompactLines::from_geo(&mls);
        assert_eq!(cl.len(), 2);
        assert_eq!(cl.to_geo(), mls);
        assert_eq!(
            quantize(c(1.23456789, -0.00000004)),
            LatpLon {
                latp: 0,
                lon: 12_345_679
            }
        );
    }
}
