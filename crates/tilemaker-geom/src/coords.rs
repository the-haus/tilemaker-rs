//! Projection and tile-coordinate maths (port of `coordinates.cpp`).

use std::f64::consts::PI;

/// Earth radius used for areas and lengths, in metres (as tilemaker).
pub const RADIUS_METER: f64 = 6_371_000.0;
/// Largest projected latitude.
pub const MAX_LATP: f64 = 180.0;
/// Smallest projected latitude.
pub const MIN_LATP: f64 = -MAX_LATP;

/// A position at 1e-7 degree precision with a projected latitude.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LatpLon {
    /// Projected latitude × 1e7.
    pub latp: i32,
    /// Longitude × 1e7.
    pub lon: i32,
}

impl LatpLon {
    /// From WGS84 latitude/longitude at 1e-7 degrees (OSM's fixed point).
    #[inline]
    pub fn from_lat_lon_e7(lat: i32, lon: i32) -> Self {
        Self {
            latp: (lat2latp(lat as f64 / 1e7) * 1e7) as i32,
            lon,
        }
    }

    /// From WGS84 latitude/longitude in degrees.
    #[inline]
    pub fn from_degrees(lat: f64, lon: f64) -> Self {
        Self {
            latp: (lat2latp(lat) * 1e7) as i32,
            lon: (lon * 1e7) as i32,
        }
    }

    /// WGS84 latitude in degrees.
    #[inline]
    pub fn lat(self) -> f64 {
        latp2lat(self.latp as f64 / 1e7)
    }

    /// Longitude in degrees.
    #[inline]
    pub fn lon_deg(self) -> f64 {
        self.lon as f64 / 1e7
    }
}

/// A tile x/y at some zoom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TileCoord {
    /// Column.
    pub x: u32,
    /// Row (XYZ scheme, 0 at the top).
    pub y: u32,
}

impl TileCoord {
    /// A tile coordinate.
    #[inline]
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }

    /// The tile containing this one `dz` zoom levels up.
    #[inline]
    pub const fn parent(self, dz: u8) -> Self {
        Self {
            x: self.x >> dz,
            y: self.y >> dz,
        }
    }
}

#[inline]
fn clamp(value: f64, limit: f64) -> f64 {
    value.clamp(-limit, limit)
}

/// Degrees to radians.
#[inline]
pub fn deg2rad(deg: f64) -> f64 {
    (PI / 180.0) * deg
}

/// Radians to degrees.
#[inline]
pub fn rad2deg(rad: f64) -> f64 {
    (180.0 / PI) * rad
}

/// Project a latitude (spherical Mercator, in degrees).
#[inline]
pub fn lat2latp(lat: f64) -> f64 {
    rad2deg((deg2rad(clamp(lat, 85.06) + 90.0) / 2.0).tan().ln())
}

/// Unproject a latitude.
#[inline]
pub fn latp2lat(latp: f64) -> f64 {
    rad2deg(deg2rad(latp).exp().atan() * 2.0) - 90.0
}

/// Fractional tile column of a longitude.
#[inline]
pub fn lon2tilexf(lon: f64, z: u8) -> f64 {
    (lon + 180.0) * (1.0 / 360.0) * (1u64 << z) as f64
}

/// Fractional tile row of a projected latitude.
#[inline]
pub fn latp2tileyf(latp: f64, z: u8) -> f64 {
    (180.0 - latp) * (1.0 / 360.0) * (1u64 << z) as f64
}

/// Fractional tile row of a latitude.
#[inline]
pub fn lat2tileyf(lat: f64, z: u8) -> f64 {
    latp2tileyf(lat2latp(lat), z)
}

/// Tile column of a longitude.
#[inline]
pub fn lon2tilex(lon: f64, z: u8) -> u32 {
    clamp_tile(lon2tilexf(lon, z), z)
}

/// Tile row of a projected latitude.
#[inline]
pub fn latp2tiley(latp: f64, z: u8) -> u32 {
    clamp_tile(latp2tileyf(latp, z), z)
}

/// Tile row of a latitude.
#[inline]
pub fn lat2tiley(lat: f64, z: u8) -> u32 {
    clamp_tile(lat2tileyf(lat, z), z)
}

#[inline]
fn clamp_tile(f: f64, z: u8) -> u32 {
    let max = ((1u64 << z) - 1) as f64;
    f.clamp(0.0, max) as u32
}

/// Western longitude of a tile column.
#[inline]
pub fn tilex2lon(x: u32, z: u8) -> f64 {
    x as f64 / (1u64 << z) as f64 * 360.0 - 180.0
}

/// Northern projected latitude of a tile row.
#[inline]
pub fn tiley2latp(y: u32, z: u8) -> f64 {
    180.0 - y as f64 / (1u64 << z) as f64 * 360.0
}

/// Northern latitude of a tile row.
#[inline]
pub fn tiley2lat(y: u32, z: u8) -> f64 {
    latp2lat(tiley2latp(y, z))
}

/// The base-zoom tile containing a position.
#[inline]
pub fn latplon2index(ll: LatpLon, base_zoom: u8) -> TileCoord {
    TileCoord::new(
        lon2tilex(ll.lon as f64 / 1e7, base_zoom),
        latp2tiley(ll.latp as f64 / 1e7, base_zoom),
    )
}

/// A distance in projected degrees to metres at a projected latitude.
#[inline]
pub fn degp2meter(degp: f64, latp: f64) -> f64 {
    RADIUS_METER * deg2rad(degp) * deg2rad(latp2lat(latp)).cos()
}

/// A distance in metres to projected degrees at a projected latitude.
#[inline]
pub fn meter2degp(meter: f64, latp: f64) -> f64 {
    rad2deg((1.0 / RADIUS_METER) * (meter / deg2rad(latp2lat(latp)).cos()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projection_round_trips() {
        for lat in [-85.0, -45.5, 0.0, 12.345, 51.5, 85.0] {
            assert!((latp2lat(lat2latp(lat)) - lat).abs() < 1e-9, "{lat}");
        }
        assert!(lat2latp(0.0).abs() < 1e-12);
        // Clamped to ±85.06.
        assert!((lat2latp(89.0) - lat2latp(85.06)).abs() < 1e-12);
    }

    #[test]
    fn tile_indices() {
        assert_eq!(lon2tilex(-180.0, 0), 0);
        assert_eq!(lon2tilex(179.999, 1), 1);
        assert_eq!(lat2tiley(60.0, 1), 0);
        assert_eq!(lat2tiley(-60.0, 1), 1);
        // London at z14.
        assert_eq!(lon2tilex(-0.1278, 14), 8186);
        assert_eq!(lat2tiley(51.5074, 14), 5448);
        assert!((tilex2lon(8186, 14) - -0.1318359375).abs() < 1e-9);
        assert!((tiley2lat(0, 0) - 85.0511287798).abs() < 1e-6);
    }

    #[test]
    fn index_from_latplon() {
        let ll = LatpLon::from_degrees(47.14, 9.52);
        assert_eq!(latplon2index(ll, 14), TileCoord::new(8625, 5753));
        assert_eq!(TileCoord::new(8625, 5753).parent(2), TileCoord::new(2156, 1438));
    }

    #[test]
    fn metres() {
        let d = meter2degp(1000.0, lat2latp(45.0));
        assert!((degp2meter(d, lat2latp(45.0)) - 1000.0).abs() < 1e-6);
    }
}
