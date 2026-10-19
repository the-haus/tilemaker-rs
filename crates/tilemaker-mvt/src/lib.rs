//! Mapbox Vector Tile 2.1 encoder and decoder.
//!
//! The protobuf wire format is hand-rolled; there are no external
//! dependencies. [`TileBuilder`] produces tiles, [`decode`] reads them back
//! (used for tests and for merging into existing tiles).

mod decode;
mod encode;
mod pbf;

pub use decode::{decode, DecodeError, DecodedFeature, DecodedLayer, DecodedTile};
pub use encode::{FeatureBuilder, LayerBuilder, TileBuilder};

/// Geometry type of a feature.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GeomType {
    /// Unknown / unset geometry type (only produced by the decoder).
    Unknown = 0,
    /// (Multi)point.
    Point = 1,
    /// (Multi)linestring.
    LineString = 2,
    /// (Multi)polygon.
    Polygon = 3,
}

impl GeomType {
    /// Converts the protobuf enum value; unrecognised values map to `Unknown`.
    pub fn from_u32(v: u32) -> GeomType {
        match v {
            1 => GeomType::Point,
            2 => GeomType::LineString,
            3 => GeomType::Polygon,
            _ => GeomType::Unknown,
        }
    }
}

/// A property value as stored in a layer's value table.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `string_value` (field 1).
    String(String),
    /// `float_value` (field 2).
    Float(f32),
    /// `double_value` (field 3).
    Double(f64),
    /// `int_value` (field 4).
    Int(i64),
    /// `uint_value` (field 5).
    UInt(u64),
    /// `sint_value` (field 6, zigzag encoded).
    SInt(i64),
    /// `bool_value` (field 7).
    Bool(bool),
}

/// Geometry command ids.
pub(crate) const CMD_MOVE_TO: u32 = 1;
pub(crate) const CMD_LINE_TO: u32 = 2;
pub(crate) const CMD_CLOSE_PATH: u32 = 7;

/// Builds a geometry command integer.
#[inline]
pub(crate) fn command(id: u32, count: u32) -> u32 {
    (id & 7) | (count << 3)
}

#[cfg(test)]
mod tests;
