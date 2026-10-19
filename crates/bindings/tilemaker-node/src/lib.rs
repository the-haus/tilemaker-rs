#![deny(clippy::all)]

//! Node.js bindings: stream elements and assembled features out of
//! OpenStreetMap PBF files.
//!
//! Exports run on their own threads (decoding on all cores); results cross
//! into JS as columnar batches under credit-based flow control (see
//! [`credits`]), so a slow consumer bounds memory instead of filling it.

mod batch;
mod credits;
mod options;
mod stream;
mod tiles;

use napi::bindgen_prelude::AsyncTask;
use napi::{Env, JsFunction, Result, Task};
use napi_derive::napi;
use tilemaker_export::{export_elements, export_features, PbfFile};

use batch::{ElementBatch, FeatureBatch};
use options::invalid;
pub use options::StreamOptions;
pub use stream::NativeStream;
use stream::StreamEvent;
pub use tiles::TileStreamOptions;

/// The file header.
#[napi(object)]
pub struct PbfHeader {
    /// `[west, south, east, north]`, when recorded.
    pub bbox: Option<Vec<f64>>,
    /// Required features.
    pub required_features: Vec<String>,
    /// Optional features.
    pub optional_features: Vec<String>,
    /// Writing program.
    pub writing_program: Option<String>,
    /// Source.
    pub source: Option<String>,
    /// Replication timestamp (seconds since the epoch).
    pub replication_timestamp: Option<f64>,
    /// Replication sequence number.
    pub replication_sequence_number: Option<f64>,
    /// Replication base URL.
    pub replication_base_url: Option<String>,
    /// Sorted by type, then id.
    pub sorted: bool,
    /// Ways carry node locations.
    pub locations_on_ways: bool,
}

/// An open PBF file.
#[napi]
pub struct NativeReader {
    file: PbfFile,
}

#[napi]
impl NativeReader {
    /// The header.
    #[napi(getter)]
    pub fn header(&self) -> PbfHeader {
        let h = self.file.header();
        PbfHeader {
            bbox: h.bbox.map(|b| vec![b.min_lon, b.min_lat, b.max_lon, b.max_lat]),
            required_features: h.required_features.clone(),
            optional_features: h.optional_features.clone(),
            writing_program: h.writing_program.clone(),
            source: h.source.clone(),
            replication_timestamp: h.replication_timestamp.map(|t| t as f64),
            replication_sequence_number: h.replication_sequence_number.map(|t| t as f64),
            replication_base_url: h.replication_base_url.clone(),
            sorted: h.is_sorted(),
            locations_on_ways: h.has_locations_on_ways(),
        }
    }

    /// Number of data blocks.
    #[napi(getter)]
    pub fn block_count(&self) -> u32 {
        self.file.blobs().len() as u32
    }

    /// File size in bytes.
    #[napi(getter)]
    pub fn size(&self) -> f64 {
        self.file.len() as f64
    }

    /// The path.
    #[napi(getter)]
    pub fn path(&self) -> String {
        self.file.path().display().to_string()
    }

    /// Stream assembled features to `callback(event)`.
    #[napi(ts_args_type = "options: StreamOptions, callback: (event: unknown) => void")]
    pub fn features(&self, options: StreamOptions, callback: JsFunction) -> Result<NativeStream> {
        let opts = options::features(&options)?;
        let cb = stream::callback(&callback)?;
        let file = self.file.clone();
        Ok(stream::spawn::<FeatureBatch, _>(
            cb,
            options.batch_size.unwrap_or(4096) as usize,
            options.read_ahead.unwrap_or(2),
            StreamEvent::Features,
            move |sink| export_features(&file, &opts, sink).map_err(|e| e.to_string()),
        ))
    }

    /// Stream raw elements to `callback(event)`.
    #[napi(ts_args_type = "options: StreamOptions, callback: (event: unknown) => void")]
    pub fn elements(&self, options: StreamOptions, callback: JsFunction) -> Result<NativeStream> {
        let opts = options::elements(&options)?;
        let cb = stream::callback(&callback)?;
        let file = self.file.clone();
        Ok(stream::spawn::<ElementBatch, _>(
            cb,
            options.batch_size.unwrap_or(4096) as usize,
            options.read_ahead.unwrap_or(2),
            StreamEvent::Elements,
            move |sink| export_elements(&file, &opts, sink).map_err(|e| e.to_string()),
        ))
    }
}

#[napi]
impl NativeReader {
    /// Generate vector tiles, streaming `{ z, x, y, data }` batches.
    #[napi(ts_args_type = "options: TileStreamOptions, callback: (event: unknown) => void")]
    pub fn tiles(&self, options: TileStreamOptions, callback: JsFunction) -> Result<NativeStream> {
        let prepared = tiles::prepare(&options, true)?;
        let cb = stream::callback(&callback)?;
        Ok(tiles::spawn_tiles(
            self.file.clone(),
            prepared,
            options.batch_size.unwrap_or(64).max(1) as usize,
            options.read_ahead.unwrap_or(2),
            cb,
        ))
    }

    /// Generate vector tiles into `options.output`.
    #[napi(ts_args_type = "options: TileStreamOptions, callback: (event: unknown) => void")]
    pub fn write_tiles(&self, options: TileStreamOptions, callback: JsFunction) -> Result<NativeStream> {
        let output = options.output.clone().ok_or_else(|| invalid("output is required"))?;
        let prepared = tiles::prepare(&options, false)?;
        let cb = stream::callback(&callback)?;
        Ok(tiles::spawn_write(self.file.clone(), prepared, output, cb))
    }
}

/// Opens and indexes a file on the libuv pool.
pub struct OpenTask {
    path: String,
}

impl Task for OpenTask {
    type Output = PbfFile;
    type JsValue = NativeReader;

    fn compute(&mut self) -> Result<PbfFile> {
        PbfFile::open(&self.path).map_err(|e| invalid(format!("{}: {e}", self.path)))
    }

    fn resolve(&mut self, _env: Env, file: PbfFile) -> Result<NativeReader> {
        Ok(NativeReader { file })
    }
}

/// Open a PBF file (indexes its blocks; reads only blob headers).
#[napi(ts_return_type = "Promise<NativeReader>")]
pub fn open(path: String) -> AsyncTask<OpenTask> {
    AsyncTask::new(OpenTask { path })
}

/// The library version.
#[napi]
pub fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
