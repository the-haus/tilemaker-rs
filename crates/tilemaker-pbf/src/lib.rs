#![doc = include_str!("../README.md")]
//!
//! A fast, streaming reader (and simple writer) for OpenStreetMap PBF files.
//!
//! * [`PbfFile`] indexes a file's blobs and decodes blocks in parallel,
//!   in file order, with bounded memory ([`PbfFile::blocks`]).
//! * [`BlobReader`] reads blobs sequentially from any [`std::io::Read`].
//! * [`PrimitiveBlock`] holds a decoded block; its [`Node`], [`Way`] and
//!   [`Relation`] views borrow strings from the block's string table.
//! * [`PbfWriter`] writes PBF files (used heavily by the tests).
//!
//! Supported: dense and plain nodes, ways (with `LocationsOnWays`),
//! relations, metadata, raw and zlib blobs. Not supported: lzma, lz4 and
//! zstd blobs (rare in practice; reported as [`Error::Unsupported`]).

mod blob;
mod block;
mod file;
mod header;
pub mod parallel;
mod proto;
pub mod write;

pub use blob::{decompress_blob, BlobInfo, BlobKind, BlobReader};
pub use block::{
    BlockKinds, DecodeOptions, ElementType, Info, InfoRef, Member, Node, PrimitiveBlock, Relation, Tags, Way,
};
pub use file::{Layout, PbfFile};
pub use header::{Header, HeaderBbox};
pub use write::{PbfWriter, WriterOptions};

/// Errors reading a PBF file.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// An I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Malformed data.
    #[error("corrupt PBF data: {0}")]
    Corrupt(&'static str),
    /// A blob failed to decompress.
    #[error("decompression failed: {0}")]
    Decompress(String),
    /// A feature this reader does not implement.
    #[error("unsupported PBF feature: {0}")]
    Unsupported(String),
}

/// Decode a header from a decompressed `OSMHeader` blob.
pub fn decode_header(data: &[u8]) -> Result<Header, Error> {
    Header::decode(data)
}
