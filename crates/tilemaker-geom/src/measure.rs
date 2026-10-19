//! Areas, lengths, centroids and label points (as tilemaker's Lua API
//! reports them).

use geo::Centroid;

use crate::coords::{deg2rad, latp2lat, RADIUS_METER};
use crate::{Coord, LineString, MultiPolygon, Point, Polygon};

/// Spherical area in square metres of a ring given in lon/latp.
fn ring_area_m2(ring: &[Coord]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    // Chamberlain & Duquette, "Some algorithms for polygons on a sphere".
    let mut total = 0.0;
    let n = ring.len();
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        let (lat1, lat2) = (deg2rad(latp2lat(a.y)), deg2rad(latp2lat(b.y)));
        total += deg2rad(b.x - a.x) * (2.0 + lat1.sin() + lat2.sin());
    }
    (total * RADIUS_METER * RADIUS_METER / 2.0).abs()
}

/// Spherical area of a polygon in square metres (holes subtracted).
pub fn polygon_area_m2(p: &Polygon) -> f64 {
    let outer = ring_area_m2(&p.exterior().0);
    let holes: f64 = p.interiors().iter().map(|r| ring_area_m2(&r.0)).sum();
    (outer - holes).max(0.0)
}

/// Spherical area of a multipolygon in square metres.
pub fn multipolygon_area_m2(mp: &MultiPolygon) -> f64 {
    mp.0.iter().map(polygon_area_m2).sum()
}

/// Haversine length in metres of a linestring given in lon/latp.
pub fn linestring_length_m(ls: &LineString) -> f64 {
    ls.0.windows(2).map(|w| haversine_m(w[0], w[1])).sum()
}

fn haversine_m(a: Coord, b: Coord) -> f64 {
    let (lat1, lat2) = (deg2rad(latp2lat(a.y)), deg2rad(latp2lat(b.y)));
    let dlat = lat2 - lat1;
    let dlon = deg2rad(b.x - a.x);
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * RADIUS_METER * h.sqrt().asin()
}

/// Centroid of a linestring (a single-point line is its point).
pub fn linestring_centroid(ls: &LineString) -> Option<Point> {
    match ls.0.len() {
        0 => None,
        1 => Some(Point::from(ls.0[0])),
        _ => ls.centroid().or_else(|| Some(Point::from(ls.0[0]))),
    }
}

/// Centroid of a polygon.
pub fn polygon_centroid(p: &Polygon) -> Option<Point> {
    p.centroid()
}

/// Centroid of a multipolygon.
pub fn multipolygon_centroid(mp: &MultiPolygon) -> Option<Point> {
    mp.centroid()
}

/// Pole of inaccessibility of a polygon (a label point inside it).
pub fn polygon_polylabel(p: &Polygon) -> Option<Point> {
    geo::BoundingRect::bounding_rect(p)?;
    // tilemaker's precision: 0.00001° (about a metre).
    polylabel::polylabel(p, &0.00001).ok()
}

/// Pole of inaccessibility of the largest polygon in a multipolygon.
pub fn multipolygon_polylabel(mp: &MultiPolygon) -> Option<Point> {
    let biggest =
        mp.0.iter()
            .max_by(|a, b| geo::Area::unsigned_area(*a).total_cmp(&geo::Area::unsigned_area(*b)))?;
    polygon_polylabel(biggest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coords::lat2latp;

    fn c(lon: f64, lat: f64) -> Coord {
        Coord {
            x: lon,
            y: lat2latp(lat),
        }
    }

    #[test]
    fn one_degree_square_at_equator() {
        let p = Polygon::new(
            LineString::new(vec![c(0.0, 0.0), c(1.0, 0.0), c(1.0, 1.0), c(0.0, 1.0), c(0.0, 0.0)]),
            vec![],
        );
        let a = polygon_area_m2(&p);
        // ≈ 12,363 km² on a 6371 km sphere.
        assert!((a / 1e6 - 12_363.0).abs() < 10.0, "{a}");
    }

    #[test]
    fn length_of_one_degree_of_latitude() {
        let ls = LineString::new(vec![c(0.0, 0.0), c(0.0, 1.0)]);
        let l = linestring_length_m(&ls);
        assert!((l - 111_194.9).abs() < 1.0, "{l}");
    }

    #[test]
    fn label_point_is_inside() {
        // A U shape whose centroid lies outside it.
        let p = Polygon::new(
            LineString::new(vec![
                Coord { x: 0.0, y: 0.0 },
                Coord { x: 3.0, y: 0.0 },
                Coord { x: 3.0, y: 3.0 },
                Coord { x: 2.0, y: 3.0 },
                Coord { x: 2.0, y: 1.0 },
                Coord { x: 1.0, y: 1.0 },
                Coord { x: 1.0, y: 3.0 },
                Coord { x: 0.0, y: 3.0 },
                Coord { x: 0.0, y: 0.0 },
            ]),
            vec![],
        );
        let pt = polygon_polylabel(&p).unwrap();
        assert!(geo::Contains::contains(&p, &pt));
    }
}
