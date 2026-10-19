//! The `OSMHeader` block.

use crate::proto::{Reader, LEN};
use crate::Error;

/// Bounding box from the file header, in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HeaderBbox {
    /// West.
    pub min_lon: f64,
    /// South.
    pub min_lat: f64,
    /// East.
    pub max_lon: f64,
    /// North.
    pub max_lat: f64,
}

/// The file header.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    /// Bounding box, when the writer recorded one.
    pub bbox: Option<HeaderBbox>,
    /// Features a reader must support.
    pub required_features: Vec<String>,
    /// Optional features (e.g. `Sort.Type_then_ID`, `LocationsOnWays`).
    pub optional_features: Vec<String>,
    /// The program that wrote the file.
    pub writing_program: Option<String>,
    /// Data source.
    pub source: Option<String>,
    /// Replication timestamp (seconds since the epoch).
    pub replication_timestamp: Option<i64>,
    /// Replication sequence number.
    pub replication_sequence_number: Option<i64>,
    /// Replication base URL.
    pub replication_base_url: Option<String>,
}

/// Features this reader understands.
pub const SUPPORTED_FEATURES: &[&str] = &["OsmSchema-V0.6", "DenseNodes", "HistoricalInformation"];

impl Header {
    /// Decode a header block.
    pub fn decode(data: &[u8]) -> Result<Self, Error> {
        let mut h = Header::default();
        let mut r = Reader::new(data);
        let string = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        while let Some((field, wire)) = r.field()? {
            match (field, wire) {
                (1, LEN) => {
                    let mut b = Reader::new(r.bytes()?);
                    let (mut l, mut rt, mut t, mut bt) = (0i64, 0i64, 0i64, 0i64);
                    while let Some((f, w)) = b.field()? {
                        match f {
                            1 => l = b.sint64()?,
                            2 => rt = b.sint64()?,
                            3 => t = b.sint64()?,
                            4 => bt = b.sint64()?,
                            _ => b.skip(w)?,
                        }
                    }
                    h.bbox = Some(HeaderBbox {
                        min_lon: l as f64 / 1e9,
                        max_lon: rt as f64 / 1e9,
                        max_lat: t as f64 / 1e9,
                        min_lat: bt as f64 / 1e9,
                    });
                }
                (4, LEN) => h.required_features.push(string(r.bytes()?)),
                (5, LEN) => h.optional_features.push(string(r.bytes()?)),
                (16, LEN) => h.writing_program = Some(string(r.bytes()?)),
                (17, LEN) => h.source = Some(string(r.bytes()?)),
                (32, _) => h.replication_timestamp = Some(r.int64()?),
                (33, _) => h.replication_sequence_number = Some(r.int64()?),
                (34, LEN) => h.replication_base_url = Some(string(r.bytes()?)),
                _ => r.skip(wire)?,
            }
        }
        Ok(h)
    }

    /// Fail on required features this reader does not implement.
    pub fn check_supported(&self) -> Result<(), Error> {
        for f in &self.required_features {
            if !SUPPORTED_FEATURES.contains(&f.as_str()) {
                return Err(Error::Unsupported(format!("required feature {f}")));
            }
        }
        Ok(())
    }

    /// Whether elements are sorted by type, then id.
    pub fn is_sorted(&self) -> bool {
        self.optional_features.iter().any(|f| f == "Sort.Type_then_ID")
    }

    /// Whether ways carry their node locations.
    pub fn has_locations_on_ways(&self) -> bool {
        self.optional_features.iter().any(|f| f == "LocationsOnWays")
    }
}
