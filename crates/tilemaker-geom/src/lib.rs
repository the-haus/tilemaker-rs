//! Geometry for tilemaker.
//!
//! Coordinates follow tilemaker's conventions: longitudes are plain degrees
//! and latitudes are *projected* ("latp"), i.e. spherical-Mercator y scaled
//! back into degrees, so that a straight line in latp/lon space is a straight
//! line on the map. Fixed-point positions ([`LatpLon`]) store both at 1e-7
//! degrees, like OSM.
//!
//! Floating-point geometries ([`Point`], [`LineString`], [`MultiPolygon`] …)
//! are the [`geo`] types with `x = lon` and `y = latp`.

pub mod assemble;
pub mod bbox;
pub mod clip;
pub mod coords;
pub mod measure;
pub mod simplify;
pub mod tiles;
pub mod valid;

pub use bbox::TileBbox;
pub use coords::*;

pub use geo;

/// A coordinate in lon/latp degrees.
pub type Coord = geo::Coord<f64>;
/// A point in lon/latp degrees.
pub type Point = geo::Point<f64>;
/// A linestring in lon/latp degrees.
pub type LineString = geo::LineString<f64>;
/// A multi-linestring in lon/latp degrees.
pub type MultiLineString = geo::MultiLineString<f64>;
/// A polygon in lon/latp degrees.
pub type Polygon = geo::Polygon<f64>;
/// A multipolygon in lon/latp degrees.
pub type MultiPolygon = geo::MultiPolygon<f64>;
/// An axis-aligned box in lon/latp degrees.
pub type Rect = geo::Rect<f64>;

/// Convert a fixed-point position to a floating-point coordinate.
#[inline]
pub fn ll_to_coord(ll: LatpLon) -> Coord {
    Coord {
        x: ll.lon as f64 / 1e7,
        y: ll.latp as f64 / 1e7,
    }
}

/// Convert a run of fixed-point positions to a linestring.
pub fn ll_to_linestring(lls: &[LatpLon]) -> LineString {
    LineString::new(lls.iter().map(|&ll| ll_to_coord(ll)).collect())
}
