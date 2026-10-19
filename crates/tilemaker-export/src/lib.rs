//! Stream filtered OpenStreetMap data out of PBF files.
//!
//! * [`export_elements`]: raw nodes, ways and relations matching
//!   osmium-style tag filters, optionally with way node locations.
//! * [`export_features`]: assembled geometries — points, linestrings,
//!   multi-linestrings and multipolygons (from closed ways and
//!   multipolygon/boundary relations) — like `osmium export`.
//!
//! Both run on all cores, keep only what the selection needs in memory
//! (see [`engine`]), and hand records to a sink block by block.

pub mod area;
pub mod engine;
pub mod filter;
pub mod record;

pub use area::ClosedWays;
pub use engine::{
    export_elements, export_features, CommonOptions, ElementOptions, Event, ExportStats, FeatureOptions, Flow,
    GeometryTypes, NodeStorage, Phase, Progress,
};
pub use filter::{FilterParseError, FilterSet, TagFilter};
pub use record::{Element, Feature, Geometry, GeometryType, MemberRecord, Meta, TagSelection};
pub use tilemaker_pbf::{ElementType, PbfFile};

/// Errors from an export.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// Reading the PBF file failed.
    #[error(transparent)]
    Pbf(#[from] tilemaker_pbf::Error),
    /// The node store failed.
    #[error(transparent)]
    Store(#[from] tilemaker_store::StoreError),
    /// A bad filter.
    #[error(transparent)]
    Filter(#[from] FilterParseError),
}
