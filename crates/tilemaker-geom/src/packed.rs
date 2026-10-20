//! Geometries as varint byte records: how tile data keeps millions of lines
//! and polygons without an allocation each, at about 3 bytes a point
//! instead of 8.
//!
//! All numbers are varints; positions are zigzag deltas at 1e-7 degrees,
//! each from the previous position (the first from 0, or from its
//! polygon's box corner).
//!
//! * line: `n`, then `n` positions;
//! * lines: the number of lines, each line's `n`, then all positions;
//! * polygons: the number of polygons, then per polygon: the number of
//!   rings, each ring's `n << 1 | closed` (a closed ring's last position,
//!   equal to its first, is not stored); when there is more than one
//!   polygon, its box (`min lon, min latp` from the previous box's corner,
//!   width, height) and the byte length of its positions, so polygons
//!   outside a tile are skipped undecoded; then its positions.

use crate::compact::{CompactLines, CompactPolygons};
use crate::coords::LatpLon;
use crate::{Coord, LineString, MultiLineString, MultiPolygon, Polygon};

#[inline]
fn put(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

#[inline]
fn zz(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

#[inline]
fn unzz(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Positions as deltas from `prev` (updated to the last one).
fn put_positions(out: &mut Vec<u8>, pts: &[LatpLon], prev: &mut (i64, i64)) {
    for p in pts {
        put(out, zz(p.lon as i64 - prev.0));
        put(out, zz(p.latp as i64 - prev.1));
        *prev = (p.lon as i64, p.latp as i64);
    }
}

/// Encode a line.
pub fn encode_line(pts: &[LatpLon], out: &mut Vec<u8>) {
    put(out, pts.len() as u64);
    put_positions(out, pts, &mut (0, 0));
}

/// Encode several lines.
pub fn encode_lines(lines: &CompactLines, out: &mut Vec<u8>) {
    put(out, lines.len() as u64);
    for l in lines.lines() {
        put(out, l.len() as u64);
    }
    put_positions(out, &lines.coords, &mut (0, 0));
}

/// Encode polygons.
pub fn encode_polygons(p: &CompactPolygons, out: &mut Vec<u8>) {
    let multi = p.len() > 1;
    put(out, p.len() as u64);
    let mut corner = (0i64, 0i64);
    let mut body = Vec::new();
    for (rings, b) in p.polygons().zip(p.boxes.iter()) {
        put(out, rings.len() as u64);
        let closed = |r: &[LatpLon]| r.len() > 1 && r[0] == r[r.len() - 1];
        for r in &rings {
            let c = closed(r);
            put(out, ((r.len() - c as usize) as u64) << 1 | c as u64);
        }
        let mut prev = (0, 0);
        if multi {
            put(out, zz(b[0] as i64 - corner.0));
            put(out, zz(b[1] as i64 - corner.1));
            put(out, (b[2] as i64 - b[0] as i64) as u64);
            put(out, (b[3] as i64 - b[1] as i64) as u64);
            corner = (b[0] as i64, b[1] as i64);
            prev = corner;
        }
        body.clear();
        for r in &rings {
            let n = r.len() - closed(r) as usize;
            put_positions(&mut body, &r[..n], &mut prev);
        }
        if multi {
            put(out, body.len() as u64);
        }
        out.extend_from_slice(&body);
    }
}

/// Reads a record.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    #[inline]
    fn get(&mut self) -> u64 {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.buf[self.pos];
            self.pos += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return v;
            }
            shift += 7;
        }
    }

    #[inline]
    fn positions(&mut self, n: usize, prev: &mut (i64, i64), out: &mut Vec<Coord>) {
        out.reserve(n);
        for _ in 0..n {
            prev.0 += unzz(self.get());
            prev.1 += unzz(self.get());
            out.push(Coord {
                x: prev.0 as f64 / 1e7,
                y: prev.1 as f64 / 1e7,
            });
        }
    }
}

/// The line in a record, as coordinates (into `out`, cleared first).
pub fn decode_line(rec: &[u8], out: &mut Vec<Coord>) {
    out.clear();
    let mut r = Reader { buf: rec, pos: 0 };
    let n = r.get() as usize;
    r.positions(n, &mut (0, 0), out);
}

/// Call `f` with each line of a lines record.
pub fn for_each_line(rec: &[u8], mut f: impl FnMut(&[Coord])) {
    let mut r = Reader { buf: rec, pos: 0 };
    let count = r.get() as usize;
    let lens: Vec<usize> = (0..count).map(|_| r.get() as usize).collect();
    let mut prev = (0, 0);
    let mut line = Vec::new();
    for n in lens {
        line.clear();
        r.positions(n, &mut prev, &mut line);
        f(&line);
    }
}

/// Visit the polygons of a polygons record. For each, `wanted` sees its box
/// (`[min lon, min latp, max lon, max latp]` at 1e-7; `None` when the
/// record holds just one polygon) and says whether to decode it; `f` then
/// gets its rings, exterior first.
pub fn for_each_polygon(
    rec: &[u8],
    mut wanted: impl FnMut(Option<[i32; 4]>) -> bool,
    mut f: impl FnMut(&[Vec<Coord>]),
) {
    let mut r = Reader { buf: rec, pos: 0 };
    let count = r.get() as usize;
    let multi = count > 1;
    let mut corner = (0i64, 0i64);
    let mut ring_lens: Vec<(usize, bool)> = Vec::new();
    let mut rings: Vec<Vec<Coord>> = Vec::new();
    for _ in 0..count {
        let n = r.get() as usize;
        ring_lens.clear();
        ring_lens.extend((0..n).map(|_| {
            let v = r.get();
            ((v >> 1) as usize, v & 1 == 1)
        }));
        let mut prev = (0, 0);
        let mut bx = None;
        let mut end = None;
        if multi {
            let x0 = corner.0 + unzz(r.get());
            let y0 = corner.1 + unzz(r.get());
            let w = r.get() as i64;
            let h = r.get() as i64;
            corner = (x0, y0);
            prev = corner;
            bx = Some([x0 as i32, y0 as i32, (x0 + w) as i32, (y0 + h) as i32]);
            let len = r.get() as usize;
            end = Some(r.pos + len);
        }
        if !wanted(bx) {
            if let Some(e) = end {
                r.pos = e;
                continue;
            }
            return;
        }
        rings.resize_with(ring_lens.len(), Vec::new);
        for (ring, &(n, closed)) in rings.iter_mut().zip(&ring_lens) {
            ring.clear();
            r.positions(n, &mut prev, ring);
            if closed {
                ring.push(ring[0]);
            }
        }
        f(&rings[..ring_lens.len()]);
    }
}

/// A line record as a `geo` linestring.
pub fn line_to_geo(rec: &[u8]) -> LineString {
    let mut v = Vec::new();
    decode_line(rec, &mut v);
    LineString::new(v)
}

/// A lines record as a `geo` multi-linestring.
pub fn lines_to_geo(rec: &[u8]) -> MultiLineString {
    let mut out = Vec::new();
    for_each_line(rec, |l| out.push(LineString::new(l.to_vec())));
    MultiLineString::new(out)
}

/// A polygons record as a `geo` multipolygon.
pub fn polygons_to_geo(rec: &[u8]) -> MultiPolygon {
    let mut out = Vec::new();
    for_each_polygon(
        rec,
        |_| true,
        |rings| {
            let mut it = rings.iter().map(|r| LineString::new(r.clone()));
            let ext = it.next().unwrap_or_else(|| LineString::new(Vec::new()));
            out.push(Polygon::new(ext, it.collect()));
        },
    );
    MultiPolygon::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact::quantize;

    fn c(x: f64, y: f64) -> Coord {
        Coord { x, y }
    }

    fn sq(x0: f64, y0: f64, s: f64) -> LineString {
        LineString::new(vec![
            c(x0, y0),
            c(x0 + s, y0),
            c(x0 + s, y0 + s),
            c(x0, y0 + s),
            c(x0, y0),
        ])
    }

    fn q(mp: &MultiPolygon) -> MultiPolygon {
        CompactPolygons::from_geo(mp).to_geo()
    }

    #[test]
    fn round_trips() {
        for mp in [
            MultiPolygon::new(vec![Polygon::new(
                sq(5.1234567, 52.0, 0.001),
                vec![sq(5.1236, 52.0002, 0.0001)],
            )]),
            MultiPolygon::new(vec![
                Polygon::new(
                    sq(-179.9, -85.0, 4.0),
                    vec![sq(-179.0, -84.0, 1.0), sq(-177.0, -84.0, 1.0)],
                ),
                Polygon::new(sq(10.0, 0.0, 2.0), vec![]),
                Polygon::new(sq(179.0, 85.0, 0.5), vec![]),
            ]),
            MultiPolygon::new(vec![]),
        ] {
            let cp = CompactPolygons::from_geo(&mp);
            let mut rec = Vec::new();
            encode_polygons(&cp, &mut rec);
            assert_eq!(polygons_to_geo(&rec), q(&mp));
        }
        let mls = MultiLineString::new(vec![
            sq(0.0, 0.0, 1.0),
            LineString::new(vec![c(-1.5, 2.25), c(3.0, -4.0)]),
        ]);
        let cl = CompactLines::from_geo(&mls);
        let mut rec = Vec::new();
        encode_lines(&cl, &mut rec);
        assert_eq!(lines_to_geo(&rec), cl.to_geo());
        let line: Vec<LatpLon> = [c(4.9, 52.3), c(4.91, 52.31), c(-4.9, -52.3)]
            .into_iter()
            .map(quantize)
            .collect();
        let mut rec = Vec::new();
        encode_line(&line, &mut rec);
        assert_eq!(line_to_geo(&rec).0, crate::compact::to_coords(&line));
    }

    #[test]
    fn skips_polygons_by_box() {
        let mp = MultiPolygon::new(vec![
            Polygon::new(sq(0.0, 0.0, 1.0), vec![]),
            Polygon::new(sq(10.0, 10.0, 1.0), vec![sq(10.2, 10.2, 0.1)]),
            Polygon::new(sq(20.0, 0.0, 1.0), vec![]),
        ]);
        let cp = CompactPolygons::from_geo(&mp);
        let mut rec = Vec::new();
        encode_polygons(&cp, &mut rec);
        let mut boxes = Vec::new();
        let mut decoded = Vec::new();
        for_each_polygon(
            &rec,
            |b| {
                boxes.push(b.unwrap());
                b.unwrap()[0] >= 100_000_000
            },
            |rings| decoded.push(rings.len()),
        );
        assert_eq!(boxes, cp.boxes.to_vec());
        assert_eq!(decoded, vec![2, 1]);
    }

    #[test]
    fn small_buildings_are_small() {
        // A typical building: ~10 m sides, 5 positions.
        let mp = MultiPolygon::new(vec![Polygon::new(sq(5.12345, 52.12345, 0.0001), vec![])]);
        let mut rec = Vec::new();
        encode_polygons(&CompactPolygons::from_geo(&mp), &mut rec);
        assert!(rec.len() <= 24, "{}", rec.len());
    }
}
