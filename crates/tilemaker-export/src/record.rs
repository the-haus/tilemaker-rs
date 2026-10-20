//! What the exporter emits.

use tilemaker_pbf::{ElementType, InfoRef, Tags};

use crate::filter::Pattern;

/// The kind of a feature's geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
pub enum GeometryType {
    /// A node.
    Point,
    /// A way.
    LineString,
    /// A non-area relation's member ways, joined where they touch.
    MultiLineString,
    /// A closed way or an area relation, always as a multipolygon.
    MultiPolygon,
}

impl GeometryType {
    /// The GeoJSON type name.
    pub fn as_str(self) -> &'static str {
        match self {
            GeometryType::Point => "Point",
            GeometryType::LineString => "LineString",
            GeometryType::MultiLineString => "MultiLineString",
            GeometryType::MultiPolygon => "MultiPolygon",
        }
    }
}

/// A geometry as flat arrays (which map directly onto typed arrays):
///
/// * `coords`: `lon, lat, lon, lat, …` in degrees.
/// * `parts`: for multi-linestrings and multipolygons, where each line or
///   ring ends (exclusive end, counted in points).
/// * `polygons`: for multipolygons, where each polygon ends (exclusive end,
///   counted in rings). A polygon's first ring is its exterior
///   (counter-clockwise), the rest are holes (clockwise), as RFC 7946 asks.
#[derive(Clone, Debug, PartialEq)]
pub struct Geometry {
    /// Kind.
    pub kind: GeometryType,
    /// Interleaved longitude/latitude pairs.
    pub coords: Vec<f64>,
    /// Exclusive end of each line or ring, in points.
    pub parts: Vec<u32>,
    /// Exclusive end of each polygon, in rings.
    pub polygons: Vec<u32>,
}

impl Geometry {
    /// Bounding box `[west, south, east, north]`.
    pub fn bbox(&self) -> [f64; 4] {
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for c in self.coords.chunks_exact(2) {
            b[0] = b[0].min(c[0]);
            b[1] = b[1].min(c[1]);
            b[2] = b[2].max(c[0]);
            b[3] = b[3].max(c[1]);
        }
        b
    }

    /// Number of points.
    pub fn point_count(&self) -> usize {
        self.coords.len() / 2
    }

    /// GeoJSON geometry object.
    pub fn to_geojson(&self) -> serde_json::Value {
        use serde_json::{json, Value};
        let pt = |i: usize| json!([self.coords[2 * i], self.coords[2 * i + 1]]);
        let line = |a: usize, b: usize| Value::Array((a..b).map(pt).collect());
        let coordinates = match self.kind {
            GeometryType::Point => pt(0),
            GeometryType::LineString => line(0, self.point_count()),
            GeometryType::MultiLineString => {
                let mut start = 0;
                Value::Array(
                    self.parts
                        .iter()
                        .map(|&end| {
                            let l = line(start, end as usize);
                            start = end as usize;
                            l
                        })
                        .collect(),
                )
            }
            GeometryType::MultiPolygon => {
                let mut ring_start = 0usize;
                let mut pt_start = 0usize;
                Value::Array(
                    self.polygons
                        .iter()
                        .map(|&ring_end| {
                            let rings: Vec<Value> = (ring_start..ring_end as usize)
                                .map(|r| {
                                    let end = self.parts[r] as usize;
                                    let l = line(pt_start, end);
                                    pt_start = end;
                                    l
                                })
                                .collect();
                            ring_start = ring_end as usize;
                            Value::Array(rings)
                        })
                        .collect(),
                )
            }
        };
        json!({ "type": self.kind.as_str(), "coordinates": coordinates })
    }
}

/// Element metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Meta {
    /// Version.
    pub version: i32,
    /// Seconds since the epoch.
    pub timestamp: i64,
    /// Changeset.
    pub changeset: i64,
    /// User id.
    pub uid: i32,
    /// User name.
    pub user: String,
    /// Visible (history files).
    pub visible: bool,
}

impl Meta {
    pub(crate) fn from_ref(i: Option<InfoRef<'_>>) -> Option<Self> {
        i.map(|i| Meta {
            version: i.version,
            timestamp: i.timestamp,
            changeset: i.changeset,
            uid: i.uid,
            user: i.user().to_string(),
            visible: i.visible,
        })
    }
}

/// Which tags to emit.
#[derive(Clone, Debug, Default)]
pub enum TagSelection {
    /// Every tag.
    #[default]
    All,
    /// Only keys matching these patterns.
    Keys(Vec<Pattern>),
    /// No tags.
    None,
}

impl TagSelection {
    /// From key patterns (`*` wildcards).
    pub fn keys<S: AsRef<str>>(keys: &[S]) -> Self {
        TagSelection::Keys(keys.iter().map(|k| Pattern::new(k.as_ref())).collect())
    }

    pub(crate) fn apply(&self, tags: &Tags<'_>) -> Vec<(String, String)> {
        match self {
            TagSelection::All => tags.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            TagSelection::Keys(keys) => tags
                .iter()
                .filter(|(k, _)| keys.iter().any(|p| p.matches(k)))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            TagSelection::None => Vec::new(),
        }
    }
}

/// An assembled feature.
#[derive(Clone, Debug, PartialEq)]
pub struct Feature {
    /// The element it was made from.
    pub kind: ElementType,
    /// The element's id.
    pub id: i64,
    /// Tags (per the tag selection).
    pub tags: Vec<(String, String)>,
    /// Geometry.
    pub geometry: Geometry,
    /// Metadata, when requested.
    pub meta: Option<Meta>,
    /// Computed values (area, length, …, parent relations), as requested.
    pub extras: crate::enrich::Extras,
}

impl Feature {
    /// A GeoJSON `Feature` with the id as `"type/id"` and tags as
    /// properties (plus `@type`/`@id`).
    pub fn to_geojson(&self) -> serde_json::Value {
        let mut props = serde_json::Map::new();
        props.insert("@type".into(), self.kind.as_str().into());
        props.insert("@id".into(), self.id.into());
        for (k, v) in &self.tags {
            props.insert(k.clone(), v.clone().into());
        }
        serde_json::json!({
            "type": "Feature",
            "id": format!("{}/{}", self.kind.as_str(), self.id),
            "properties": props,
            "geometry": self.geometry.to_geojson(),
        })
    }
}

/// A relation member.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemberRecord {
    /// Member type.
    pub kind: ElementType,
    /// Member id.
    pub id: i64,
    /// Role.
    pub role: String,
}

/// A raw element.
#[derive(Clone, Debug, PartialEq)]
pub struct Element {
    /// Type.
    pub kind: ElementType,
    /// Id.
    pub id: i64,
    /// Tags (per the tag selection).
    pub tags: Vec<(String, String)>,
    /// Nodes: `[lon, lat]`.
    pub location: Option<[f64; 2]>,
    /// Ways: node ids.
    pub refs: Vec<i64>,
    /// Ways, when locations are requested: `lon, lat, …` per node. A
    /// node whose location is unknown is `NaN, NaN`.
    pub locations: Option<Vec<f64>>,
    /// Relations: members.
    pub members: Vec<MemberRecord>,
    /// Metadata, when requested.
    pub meta: Option<Meta>,
}
