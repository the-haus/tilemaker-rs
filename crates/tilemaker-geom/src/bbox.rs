//! The extent of one output tile (port of `TileBbox`).

use crate::coords::*;
use crate::{Coord, MultiPolygon, Rect};

/// One tile's extent and the scaling from lon/latp into tile pixels.
#[derive(Clone, Debug)]
pub struct TileBbox {
    /// Tile column/row.
    pub index: TileCoord,
    /// Zoom level.
    pub zoom: u8,
    /// Whether the tile uses an 8192 extent instead of 4096.
    pub hires: bool,
    /// Whether this is the configured maximum zoom.
    pub end_zoom: bool,
    /// Western longitude.
    pub min_lon: f64,
    /// Eastern longitude.
    pub max_lon: f64,
    /// Southern latitude.
    pub min_lat: f64,
    /// Northern latitude.
    pub max_lat: f64,
    /// Southern projected latitude.
    pub min_latp: f64,
    /// Northern projected latitude.
    pub max_latp: f64,
    /// Horizontal margin of the clipping box.
    pub xmargin: f64,
    /// Vertical margin of the clipping box.
    pub ymargin: f64,
    /// Degrees of longitude per tile unit.
    pub xscale: f64,
    /// Projected degrees of latitude per tile unit.
    pub yscale: f64,
    /// The tile plus a 1/200 margin: what geometries are clipped to.
    pub clipping_box: Rect,
}

impl TileBbox {
    /// The extent of tile `index` at `zoom`.
    pub fn new(index: TileCoord, zoom: u8, hires: bool, end_zoom: bool) -> Self {
        let min_lon = tilex2lon(index.x, zoom);
        let min_lat = tiley2lat(index.y + 1, zoom);
        let max_lon = tilex2lon(index.x + 1, zoom);
        let max_lat = tiley2lat(index.y, zoom);
        let min_latp = lat2latp(min_lat);
        let max_latp = lat2latp(max_lat);
        let xmargin = (max_lon - min_lon) / 200.0;
        let ymargin = (max_latp - min_latp) / 200.0;
        let extent = if hires { 8192.0 } else { 4096.0 };
        Self {
            index,
            zoom,
            hires,
            end_zoom,
            min_lon,
            max_lon,
            min_lat,
            max_lat,
            min_latp,
            max_latp,
            xmargin,
            ymargin,
            xscale: (max_lon - min_lon) / extent,
            yscale: (max_latp - min_latp) / extent,
            clipping_box: Rect::new(
                Coord {
                    x: min_lon - xmargin,
                    y: min_latp - ymargin,
                },
                Coord {
                    x: max_lon + xmargin,
                    y: max_latp + ymargin,
                },
            ),
        }
    }

    /// The tile extent in units (4096 or 8192).
    #[inline]
    pub fn extent(&self) -> u32 {
        if self.hires {
            8192
        } else {
            4096
        }
    }

    /// Scale a lon/latp position into tile units.
    #[inline]
    pub fn scale(&self, latp: f64, lon: f64) -> (i32, i32) {
        (
            ((lon - self.min_lon) / self.xscale).floor() as i32,
            ((self.max_latp - latp) / self.yscale).floor() as i32,
        )
    }

    /// Scale a coordinate into tile units.
    #[inline]
    pub fn scale_coord(&self, c: Coord) -> (i32, i32) {
        self.scale(c.y, c.x)
    }

    /// Scale a ring into tile units, with tilemaker's naive self-intersection
    /// check: if a point repeats one of the last few, backtrack to it.
    pub fn scale_ring(&self, src: &[Coord], out: &mut Vec<(i32, i32)>) {
        out.clear();
        out.reserve(src.len());
        for &c in src {
            let scaled = self.scale_coord(c);
            let mut found = false;
            for j in 1..5 {
                if out.len() < 1 + j {
                    break;
                }
                if out[out.len() - j] == scaled {
                    out.truncate(out.len() - j + 1);
                    found = true;
                    break;
                }
            }
            if !found {
                out.push(scaled);
            }
        }
    }

    /// Scale a multipolygon into tile units (as floating-point geometry),
    /// dropping rings that collapse below four points.
    pub fn scale_multipolygon(&self, src: &MultiPolygon) -> MultiPolygon {
        let mut buf = Vec::new();
        let to_ring = |buf: &Vec<(i32, i32)>| {
            geo::LineString(
                buf.iter()
                    .map(|&(x, y)| Coord {
                        x: x as f64,
                        y: y as f64,
                    })
                    .collect(),
            )
        };
        let mut polys = Vec::with_capacity(src.0.len());
        for p in &src.0 {
            self.scale_ring(&p.exterior().0, &mut buf);
            if buf.len() < 4 {
                continue;
            }
            let outer = to_ring(&buf);
            let mut inners = Vec::new();
            for r in p.interiors() {
                self.scale_ring(&r.0, &mut buf);
                if buf.len() >= 4 {
                    inners.push(to_ring(&buf));
                }
            }
            polys.push(geo::Polygon::new(outer, inners));
        }
        geo::MultiPolygon(polys)
    }

    /// A box generous enough that clipping linestrings to it never changes
    /// what is drawn in the tile (tilemaker's "extend box").
    pub fn extend_box(&self) -> Rect {
        let w = self.max_lon - self.min_lon;
        let h = self.max_latp - self.min_latp;
        Rect::new(
            Coord {
                x: self.min_lon - w * 2.0,
                y: self.min_latp - h * (8191.0 / 8192.0),
            },
            Coord {
                x: self.max_lon + w * (8191.0 / 8192.0),
                y: self.max_latp + h * 2.0,
            },
        )
    }

    /// The tile shrunk by 1/8192 on each side (tilemaker's `getTileBox`),
    /// in lon/latp: tiles that merely touch a box do not intersect it.
    pub fn tile_box(&self) -> Rect {
        let xm = (self.max_lon - self.min_lon) / 8192.0;
        let ym = (self.max_latp - self.min_latp) / 8192.0;
        Rect::new(
            Coord {
                x: self.min_lon + xm,
                y: self.min_latp + ym,
            },
            Coord {
                x: self.max_lon - xm,
                y: self.max_latp - ym,
            },
        )
    }

    /// Whether [`tile_box`](Self::tile_box) intersects a WGS84 bounding box.
    pub fn tile_box_intersects(&self, min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> bool {
        let t = self.tile_box();
        let (min_latp, max_latp) = (lat2latp(min_lat), lat2latp(max_lat));
        !(max_lon < t.min().x || min_lon > t.max().x || max_latp < t.min().y || min_latp > t.max().y)
    }

    /// Whether the tile intersects a WGS84 bounding box.
    pub fn intersects_lat_lon(&self, min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> bool {
        !(max_lon <= self.min_lon || min_lon >= self.max_lon || max_lat <= self.min_lat || min_lat >= self.max_lat)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_corners() {
        let b = TileBbox::new(TileCoord::new(1, 1), 1, false, false);
        assert_eq!(b.scale(b.max_latp, b.min_lon), (0, 0));
        let (x, y) = b.scale(b.min_latp + 1e-9, b.max_lon - 1e-9);
        assert_eq!((x, y), (4095, 4095));
        assert_eq!(b.extent(), 4096);
    }

    #[test]
    fn ring_backtracks() {
        let b = TileBbox::new(TileCoord::new(0, 0), 0, false, false);
        let pts: Vec<Coord> = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (10.0, 0.0), (0.0, 10.0)]
            .iter()
            .map(|&(x, y)| Coord { x, y })
            .collect();
        let mut out = Vec::new();
        b.scale_ring(&pts, &mut out);
        assert_eq!(out.len(), 3, "{out:?}");
    }
}
