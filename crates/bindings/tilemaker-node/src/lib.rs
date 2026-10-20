#![deny(clippy::all)]

//! Node.js bindings: stream elements and assembled features out of
//! OpenStreetMap PBF files.
//!
//! Exports run on their own threads (decoding on all cores); results cross
//! into JS as columnar batches under credit-based flow control (see
//! [`credits`]), so a slow consumer bounds memory instead of filling it.

mod batch;
mod credits;
mod input;
mod options;
mod polygon_index;
mod stream;
mod tiles;

use napi::bindgen_prelude::AsyncTask;
use napi::{Env, JsFunction, Result, Task};
use napi_derive::napi;
use tilemaker_export::{export_elements, export_features, PbfFile};

use batch::{ElementBatch, FeatureBatch};
use options::invalid;
pub use options::StreamOptions;
pub use polygon_index::NativePolygonIndex;
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

/// Options for `extract()`.
#[napi(object)]
pub struct NativeExtractOptions {
    /// The output path.
    pub output: String,
    /// osmium tags-filter expressions.
    pub filters: Option<Vec<String>>,
    /// Region: `[west, south, east, north]`.
    pub bbox: Option<Vec<f64>>,
    /// Region: GeoJSON text.
    pub polygon_json: Option<String>,
    /// Region: a `.geojson` or `.poly` file.
    pub polygon_file: Option<String>,
    /// Omit referenced nodes and members.
    pub omit_referenced: Option<bool>,
    /// Keep metadata (default true).
    pub metadata: Option<bool>,
    /// Worker threads.
    pub threads: Option<u32>,
}

/// Options for `fileInfo()`.
#[napi(object)]
pub struct FileInfoOptions {
    /// Read every element.
    pub extended: Option<bool>,
    /// Worker threads (default: all cores).
    pub threads: Option<u32>,
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
            |b| StreamEvent::Features(Box::new(b)),
            move |sink| export_features(&file, &opts, sink).map_err(|e| e.to_string()),
        ))
    }

    /// Write features as GeoJSON lines to `options.output`, serialised on
    /// the worker threads (no feature crosses into JS).
    #[napi(
        js_name = "writeGeoJSONSeq",
        ts_args_type = "options: StreamOptions, callback: (event: unknown) => void"
    )]
    pub fn write_geojson_seq(&self, options: StreamOptions, callback: JsFunction) -> Result<NativeStream> {
        let output = options.output.clone().ok_or_else(|| invalid("output is required"))?;
        let opts = options::features(&options)?;
        let geojson = tilemaker_export::GeoJsonOptions {
            record_separator: options.record_separator.unwrap_or(false),
        };
        let cb = stream::callback(&callback)?;
        let file = self.file.clone();
        Ok(stream::spawn_task(cb, move |progress| {
            let f = std::fs::File::create(&output).map_err(|e| format!("{output}: {e}"))?;
            let mut out = std::io::BufWriter::with_capacity(1 << 20, f);
            let stats = tilemaker_export::write_geojson_seq(&file, &opts, geojson, &mut out, progress)
                .map_err(|e| e.to_string())?;
            serde_json::to_value(&stats).map_err(|e| e.to_string())
        }))
    }

    /// What the file holds; with `extended`, read every element (counts,
    /// ids, bounding box, timestamps, …). Ends with the info as its stats.
    #[napi(ts_args_type = "options: StreamOptions & { extended?: boolean }, callback: (event: unknown) => void")]
    pub fn file_info(&self, options: FileInfoOptions, callback: JsFunction) -> Result<NativeStream> {
        let cb = stream::callback(&callback)?;
        let file = self.file.clone();
        Ok(stream::spawn_task(cb, move |progress| {
            let info = tilemaker_export::file_info(
                &file,
                options.extended.unwrap_or(false),
                options.threads.unwrap_or(0) as usize,
                progress,
            )
            .map_err(|e| e.to_string())?;
            Ok(info.to_json())
        }))
    }

    /// Write an extract as PBF (osmium tags-filter and/or extract).
    #[napi(ts_args_type = "options: NativeExtractOptions, callback: (event: unknown) => void")]
    pub fn extract(&self, options: NativeExtractOptions, callback: JsFunction) -> Result<NativeStream> {
        use tilemaker_export::{ExtractOptions, FilterSet, Region};
        let region = match (&options.bbox, &options.polygon_json, &options.polygon_file) {
            (Some(b), _, _) => {
                if b.len() != 4 || !b.iter().all(|v| v.is_finite()) || b[0] > b[2] || b[1] > b[3] {
                    return Err(invalid("bbox must be [west, south, east, north]"));
                }
                Some(Region::Bbox([b[0], b[1], b[2], b[3]]))
            }
            (None, Some(json), _) => {
                let v: serde_json::Value = serde_json::from_str(json).map_err(|e| invalid(e.to_string()))?;
                Some(Region::from_geojson(&v).map_err(|e| invalid(e.to_string()))?)
            }
            (None, None, Some(path)) => {
                let text = std::fs::read_to_string(path).map_err(|e| invalid(format!("{path}: {e}")))?;
                let r = if path.ends_with(".poly") {
                    Region::from_poly(&text)
                } else {
                    serde_json::from_str(&text)
                        .map_err(|e| tilemaker_export::ExportError::Invalid(e.to_string()))
                        .and_then(|v| Region::from_geojson(&v))
                };
                Some(r.map_err(|e| invalid(format!("{path}: {e}")))?)
            }
            (None, None, None) => None,
        };
        let filters = match &options.filters {
            Some(f) if !f.is_empty() => Some(FilterSet::parse(f).map_err(|e| invalid(e.to_string()))?),
            _ => None,
        };
        let opts = ExtractOptions {
            filters,
            region,
            omit_referenced: options.omit_referenced.unwrap_or(false),
            metadata: options.metadata.unwrap_or(true),
            threads: options.threads.unwrap_or(0) as usize,
            writing_program: None,
        };
        let output = options.output;
        let cb = stream::callback(&callback)?;
        let file = self.file.clone();
        Ok(stream::spawn_task(cb, move |progress| {
            let f = std::fs::File::create(&output).map_err(|e| format!("{output}: {e}"))?;
            let out = std::io::BufWriter::with_capacity(1 << 20, f);
            let (_, stats) = tilemaker_export::extract(&file, &opts, out, progress).map_err(|e| e.to_string())?;
            serde_json::to_value(&stats).map_err(|e| e.to_string())
        }))
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
            |b| StreamEvent::Elements(Box::new(b)),
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

/// Stream elements out of bytes pushed from JS (`push`, `endInput`).
#[napi(ts_args_type = "options: StreamOptions, callback: (event: unknown) => void")]
pub fn elements_from_input(options: StreamOptions, callback: JsFunction) -> Result<NativeStream> {
    let opts = options::elements(&options)?;
    let cb = stream::callback(&callback)?;
    let (sender, reader) = input::channel(cb.clone());
    let handle = stream::spawn::<ElementBatch, _>(
        cb,
        options.batch_size.unwrap_or(4096) as usize,
        options.read_ahead.unwrap_or(2),
        |b| StreamEvent::Elements(Box::new(b)),
        move |sink| tilemaker_export::export_elements_from_reader(reader, &opts, sink).map_err(|e| e.to_string()),
    );
    *handle.input.lock().unwrap() = Some(sender);
    Ok(handle)
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
