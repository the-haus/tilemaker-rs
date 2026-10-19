//! What a profile asks to be written.

use std::sync::Arc;

use tilemaker_geom::{LatpLon, LineString, MultiLineString, MultiPolygon};

/// An attribute value.
#[derive(Clone, Debug, PartialEq)]
pub enum AttributeValue {
    /// `Attribute(key, "text")`.
    String(String),
    /// `AttributeNumeric(key, n)`: written as an MVT float.
    Float(f64),
    /// `AttributeInteger(key, n)`.
    Int(i64),
    /// `AttributeBoolean(key, b)`.
    Bool(bool),
}

impl Eq for AttributeValue {}

impl std::hash::Hash for AttributeValue {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            AttributeValue::String(s) => s.hash(state),
            AttributeValue::Float(f) => f.to_bits().hash(state),
            AttributeValue::Int(i) => i.hash(state),
            AttributeValue::Bool(b) => b.hash(state),
        }
    }
}

/// One attribute of an output.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Attribute {
    /// Key.
    pub key: String,
    /// Value.
    pub value: AttributeValue,
    /// Only written from this zoom.
    pub min_zoom: u8,
}

/// The geometry of an output (lon/latp degrees).
#[derive(Clone, Debug)]
pub enum OutputGeometry {
    /// A point.
    Point(LatpLon),
    /// A way as a line.
    LineString(Arc<LineString>),
    /// A relation's ways as lines.
    MultiLineString(Arc<MultiLineString>),
    /// An area.
    MultiPolygon(Arc<MultiPolygon>),
}

impl OutputGeometry {
    /// tilemaker's geometry type code order: point, linestring,
    /// multilinestring, polygon.
    pub fn kind(&self) -> GeometryKind {
        match self {
            OutputGeometry::Point(_) => GeometryKind::Point,
            OutputGeometry::LineString(_) => GeometryKind::LineString,
            OutputGeometry::MultiLineString(_) => GeometryKind::MultiLineString,
            OutputGeometry::MultiPolygon(_) => GeometryKind::Polygon,
        }
    }
}

/// Geometry kinds, ordered as tilemaker sorts them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum GeometryKind {
    /// Point.
    Point = 0,
    /// Line.
    LineString = 1,
    /// Lines.
    MultiLineString = 2,
    /// Area.
    Polygon = 3,
}

/// One `Layer()`/`LayerAsCentroid()` call with its attributes.
#[derive(Clone, Debug)]
pub struct PendingOutput {
    /// Layer index.
    pub layer: u8,
    /// Index into [`ProcessedObject::geometries`].
    pub geometry: usize,
    /// Minimum zoom (layer minimum unless `MinZoom()` raised it).
    pub min_zoom: u8,
    /// `ZOrder()`.
    pub z_order: f32,
    /// Attributes in call order (a repeated key replaces the earlier one).
    pub attributes: Vec<Attribute>,
}

/// Everything one OSM object produced.
#[derive(Clone, Debug, Default)]
pub struct ProcessedObject {
    /// The object's (possibly `ModifyId`-ed) id.
    pub id: i64,
    /// Distinct geometries (outputs often share one).
    pub geometries: Vec<OutputGeometry>,
    /// Outputs.
    pub outputs: Vec<PendingOutput>,
}

impl ProcessedObject {
    /// Whether nothing was written.
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }
}
