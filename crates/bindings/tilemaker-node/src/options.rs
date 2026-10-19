//! JS options → export options.

use napi::bindgen_prelude::Either;
use napi::{Error, Result, Status};
use napi_derive::napi;
use tilemaker_export::{
    ClosedWays, CommonOptions, ElementOptions, FeatureOptions, FilterSet, GeometryTypes, NodeStorage, TagSelection,
};

/// Options for `features()` and `elements()`. See the TypeScript docs.
#[napi(object)]
#[derive(Default)]
pub struct StreamOptions {
    /// osmium-style tag filters, e.g. `["n/place=city,town", "r/admin_level=2,4"]`.
    pub filters: Option<Vec<String>>,
    /// `true` (default): all tags; `false`: none; a list: keys to keep (`*` wildcards).
    pub tags: Option<Either<bool, Vec<String>>>,
    /// Include version, timestamp, changeset, uid and user.
    pub metadata: Option<bool>,
    /// `[west, south, east, north]`: only what intersects it.
    pub bbox: Option<Vec<f64>>,
    /// Worker threads (default: all cores).
    pub threads: Option<u32>,
    /// A file path for a dense on-disk node location store (planet-scale
    /// selections); default: compressed in memory.
    pub node_store_file: Option<String>,
    /// Records per batch handed to JS (default 4096).
    pub batch_size: Option<u32>,
    /// Batches buffered ahead of the consumer (default 2).
    pub read_ahead: Option<u32>,
    /// Features: `"point"`, `"linestring"`, `"polygon"` (default all).
    pub geometry_types: Option<Vec<String>>,
    /// Features: what closed ways become: `"auto"` (default), `"polygon"`,
    /// `"linestring"` or `"both"`.
    pub closed_ways: Option<String>,
    /// Features: turn non-area relations (routes, …) into multi-linestrings.
    pub relation_lines: Option<bool>,
    /// Elements: which types, of `"node"`, `"way"`, `"relation"` (default all).
    pub types: Option<Vec<String>>,
    /// Elements: give ways their node coordinates.
    pub way_locations: Option<bool>,
    /// Elements: with no filters, also emit untagged elements.
    pub include_untagged: Option<bool>,
}

pub(crate) fn invalid(msg: impl Into<String>) -> Error {
    Error::new(Status::InvalidArg, msg.into())
}

pub(crate) fn common(o: &StreamOptions) -> Result<CommonOptions> {
    let mut filters = FilterSet::parse(o.filters.as_deref().unwrap_or(&[])).map_err(|e| invalid(e.to_string()))?;
    filters.include_untagged = o.include_untagged.unwrap_or(false);
    let tags = match &o.tags {
        None | Some(Either::A(true)) => TagSelection::All,
        Some(Either::A(false)) => TagSelection::None,
        Some(Either::B(keys)) => TagSelection::keys(keys),
    };
    let bbox = match &o.bbox {
        None => None,
        Some(b) if b.len() == 4 && b.iter().all(|v| v.is_finite()) && b[0] <= b[2] && b[1] <= b[3] => {
            Some([b[0], b[1], b[2], b[3]])
        }
        Some(_) => {
            return Err(invalid(
                "bbox must be [west, south, east, north] with west <= east and south <= north",
            ))
        }
    };
    Ok(CommonOptions {
        filters,
        tags,
        metadata: o.metadata.unwrap_or(false),
        bbox,
        threads: o.threads.unwrap_or(0) as usize,
        node_storage: match &o.node_store_file {
            Some(p) => NodeStorage::DenseFile(p.into()),
            None => NodeStorage::Memory,
        },
    })
}

pub(crate) fn features(o: &StreamOptions) -> Result<FeatureOptions> {
    let geometry_types = match &o.geometry_types {
        None => GeometryTypes::default(),
        Some(list) => {
            let mut g = GeometryTypes {
                points: false,
                lines: false,
                polygons: false,
            };
            for t in list {
                match t.to_ascii_lowercase().as_str() {
                    "point" => g.points = true,
                    "linestring" | "line" => g.lines = true,
                    "polygon" | "multipolygon" => g.polygons = true,
                    other => {
                        return Err(invalid(format!(
                            "unknown geometry type {other:?} (point, linestring, polygon)"
                        )))
                    }
                }
            }
            g
        }
    };
    let closed_ways = match o.closed_ways.as_deref().unwrap_or("auto") {
        "auto" => ClosedWays::Auto,
        "polygon" => ClosedWays::Polygon,
        "linestring" => ClosedWays::LineString,
        "both" => ClosedWays::Both,
        other => {
            return Err(invalid(format!(
                "unknown closedWays {other:?} (auto, polygon, linestring, both)"
            )))
        }
    };
    Ok(FeatureOptions {
        common: common(o)?,
        geometry_types,
        closed_ways,
        relation_lines: o.relation_lines.unwrap_or(false),
    })
}

pub(crate) fn elements(o: &StreamOptions) -> Result<ElementOptions> {
    let (mut nodes, mut ways, mut relations) = (true, true, true);
    if let Some(list) = &o.types {
        nodes = false;
        ways = false;
        relations = false;
        for t in list {
            match t.as_str() {
                "node" => nodes = true,
                "way" => ways = true,
                "relation" => relations = true,
                other => return Err(invalid(format!("unknown element type {other:?} (node, way, relation)"))),
            }
        }
    }
    Ok(ElementOptions {
        common: common(o)?,
        nodes,
        ways,
        relations,
        way_locations: o.way_locations.unwrap_or(false),
    })
}
