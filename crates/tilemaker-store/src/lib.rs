//! Stores for OpenStreetMap processing.
//!
//! * [`IdSetBuilder`] / [`IdSet`]: compressed sets of element ids.
//! * [`NodeStore`]: node id → location, in memory ([`NodeStore::memory`])
//!   or in a dense memory-mapped file ([`NodeStore::dense_file`]) for
//!   planet-sized inputs.
//! * [`WayStore`]: way id → node locations.
//! * [`RefStore`]: way id → node ids.
//!
//! Stores are filled concurrently (one call per decoded block), then
//! frozen for lookups.

mod ids;
mod nodes;
mod refs;
mod ways;

pub use ids::{IdSet, IdSetBuilder};
pub use nodes::Location;
pub use nodes::{NodeStore, NodeStoreBuilder};
pub use refs::{RefBatch, RefStore, RefStoreBuilder};
pub use ways::{WayBatch, WayStore, WayStoreBuilder};

/// Errors from the stores.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// An I/O error (dense file store).
    #[error("node store I/O error: {0}")]
    Io(#[from] std::io::Error),
}
