//! Tile output sinks: MBTiles, PMTiles v3 and plain directories, plus
//! compression helpers.

use std::path::Path;

mod compress;
mod directory;
mod mbtiles;
pub mod pmtiles;

pub use compress::{compress, decompress, Compression};
pub use directory::DirectoryWriter;
pub use mbtiles::MbtilesWriter;
pub use pmtiles::PmtilesWriter;

/// Errors produced by the output sinks.
#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    /// Filesystem error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite error (MBTiles output).
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// Any other error.
    #[error("{0}")]
    Other(String),
}

/// Metadata describing a finished tileset.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TilesetMetadata {
    /// Tileset name.
    pub name: String,
    /// Tileset version.
    pub version: String,
    /// Tileset description.
    pub description: String,
    /// Lowest zoom level.
    pub minzoom: u8,
    /// Highest zoom level.
    pub maxzoom: u8,
    /// `[minlon, minlat, maxlon, maxlat]`.
    pub bounds: Option<[f64; 4]>,
    /// `(lon, lat, zoom)` default view.
    pub center: Option<(f64, f64, u8)>,
    /// JSON array string used for `vector_layers`.
    pub vector_layers_json: String,
    /// Extra metadata entries (`settings.filemetadata`). Strings are stored
    /// as-is, other values JSON-encoded, where the format stores strings.
    pub extra: Vec<(String, serde_json::Value)>,
}

impl TilesetMetadata {
    /// Parses `vector_layers_json`, falling back to an empty array.
    pub(crate) fn vector_layers(&self) -> serde_json::Value {
        serde_json::from_str(&self.vector_layers_json).unwrap_or_else(|_| serde_json::Value::Array(Vec::new()))
    }

    /// Center from `center`, else derived from bounds as tilemaker does.
    pub(crate) fn effective_center(&self) -> Option<(f64, f64, u8)> {
        self.center.or_else(|| {
            self.bounds.map(|b| {
                (
                    (b[0] + b[2]) / 2.0,
                    (b[1] + b[3]) / 2.0,
                    ((u16::from(self.minzoom) + u16::from(self.maxzoom)) / 2) as u8,
                )
            })
        })
    }
}

/// Renders an extra metadata value as a string: strings as-is, others as JSON.
pub(crate) fn extra_value_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Destination for generated tiles.
pub trait TileSink: Send {
    /// `data` is the *uncompressed* MVT bytes; the sink applies its own compression policy.
    fn write_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<(), OutputError>;

    /// Writes metadata and finalises the output. Must be called once, after
    /// all tiles have been written.
    fn finish(&mut self, meta: &TilesetMetadata) -> Result<(), OutputError>;

    /// Reads back an existing tile (decompressed), for merging into existing
    /// output. Sinks that cannot read return `Ok(None)`.
    fn read_tile(&self, _z: u8, _x: u32, _y: u32) -> Result<Option<Vec<u8>>, OutputError> {
        Ok(None)
    }
}

/// Opens a sink chosen by the file extension: `.mbtiles`, `.pmtiles`, or
/// otherwise a directory. `merge` only affects MBTiles (directories always
/// overwrite individual files; PMTiles is always written fresh).
pub fn open_sink(path: &Path, compression: Compression, merge: bool) -> Result<Box<dyn TileSink>, OutputError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase());
    Ok(match ext.as_deref() {
        Some("mbtiles") => Box::new(MbtilesWriter::create(path, compression, merge)?),
        Some("pmtiles") => Box::new(PmtilesWriter::create(path, compression)?),
        _ => Box::new(DirectoryWriter::create(path, compression)?),
    })
}

/// In-memory sink for tests; stores tiles uncompressed.
#[derive(Debug, Default)]
pub struct MemorySink {
    /// Written tiles as `(z, x, y, data)`, in write order.
    pub tiles: Vec<(u8, u32, u32, Vec<u8>)>,
    /// Metadata passed to `finish`.
    pub metadata: Option<TilesetMetadata>,
}

impl MemorySink {
    /// Creates an empty sink.
    pub fn new() -> Self {
        Self::default()
    }
}

impl TileSink for MemorySink {
    fn write_tile(&mut self, z: u8, x: u32, y: u32, data: &[u8]) -> Result<(), OutputError> {
        self.tiles.push((z, x, y, data.to_vec()));
        Ok(())
    }

    fn finish(&mut self, meta: &TilesetMetadata) -> Result<(), OutputError> {
        self.metadata = Some(meta.clone());
        Ok(())
    }

    fn read_tile(&self, z: u8, x: u32, y: u32) -> Result<Option<Vec<u8>>, OutputError> {
        Ok(self
            .tiles
            .iter()
            .rev()
            .find(|t| t.0 == z && t.1 == x && t.2 == y)
            .map(|t| t.3.clone()))
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Returns a fresh, non-existent path in the system temp directory.
    pub fn temp_path(name: &str) -> PathBuf {
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        std::env::temp_dir().join(format!("tilemaker-output-test-{}-{}-{}", std::process::id(), n, name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_sink_by_extension() {
        let mb = test_util::temp_path("x.mbtiles");
        let pm = test_util::temp_path("x.pmtiles");
        let dir = test_util::temp_path("xdir");
        for p in [&mb, &pm, &dir] {
            let mut s = open_sink(p, Compression::Gzip, false).unwrap();
            s.write_tile(1, 1, 0, b"abc").unwrap();
            s.finish(&TilesetMetadata::default()).unwrap();
        }
        assert!(mb.is_file() && pm.is_file() && dir.join("1/1/0.pbf").is_file());
        let mut s = open_sink(&mb, Compression::Gzip, true).unwrap();
        assert_eq!(s.read_tile(1, 1, 0).unwrap().unwrap(), b"abc");
        s.finish(&TilesetMetadata::default()).unwrap();
        drop(s);
        std::fs::remove_file(&mb).unwrap();
        std::fs::remove_file(&pm).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn memory_sink() {
        let mut s = MemorySink::new();
        s.write_tile(0, 0, 0, b"a").unwrap();
        s.write_tile(0, 0, 0, b"b").unwrap();
        assert_eq!(s.read_tile(0, 0, 0).unwrap().unwrap(), b"b");
        s.finish(&TilesetMetadata {
            name: "m".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(s.metadata.unwrap().name, "m");
    }
}
