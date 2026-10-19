//! Tile generation: tilemaker's pipeline with tiles streamed to JS (or
//! written to MBTiles/PMTiles/a directory), under the same flow control
//! as the exports.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use napi::bindgen_prelude::{Buffer, Uint32Array, Uint8Array};
use napi::threadsafe_function::ThreadsafeFunctionCallMode;
use napi::{Env, JsObject, Result};
use napi_derive::napi;
use tilemaker_config::{Compress, Config};
use tilemaker_core::{generate, write_tiles, Event, Flow, Progress, TileOptions};
use tilemaker_output::Compression;
use tilemaker_pbf::PbfFile;

use crate::credits::Credits;
use crate::options::invalid;
use crate::stream::{Callback, NativeStream, StreamEvent};

/// Options for `tiles()` and `writeTiles()`.
#[napi(object)]
#[derive(Default)]
pub struct TileStreamOptions {
    /// The config, as JSON text.
    pub config_json: String,
    /// The Lua profile's source.
    pub process_source: String,
    /// The profile's name (for error messages).
    pub process_name: Option<String>,
    /// `[west, south, east, north]` instead of the file header's box.
    pub bbox: Option<Vec<f64>>,
    /// Worker threads (default: all cores).
    pub threads: Option<u32>,
    /// `tiles()`: compress streamed tiles: `"none"` (default), `"gzip"`,
    /// `"deflate"`, or `"config"` (the config's `compress`).
    pub compression: Option<String>,
    /// `tiles()`: tiles per batch (default 64).
    pub batch_size: Option<u32>,
    /// `tiles()`: batches buffered ahead of the consumer (default 2).
    pub read_ahead: Option<u32>,
    /// `writeTiles()`: the output path (`.mbtiles`, `.pmtiles`, or a directory).
    pub output: Option<String>,
}

pub(crate) struct Prepared {
    config: Config,
    profile: String,
    name: String,
    opts: TileOptions,
    compression: Compression,
}

fn config_compression(c: &Config) -> Compression {
    match c.compress {
        Compress::Gzip => Compression::Gzip,
        Compress::Deflate => Compression::Deflate,
        Compress::None => Compression::None,
    }
}

pub(crate) fn prepare(o: &TileStreamOptions, streaming: bool) -> Result<Prepared> {
    let config = Config::from_json(&o.config_json).map_err(|e| invalid(e.to_string()))?;
    let bbox = match &o.bbox {
        None => None,
        Some(b) if b.len() == 4 && b.iter().all(|v| v.is_finite()) && b[0] <= b[2] && b[1] <= b[3] => {
            Some([b[0], b[1], b[2], b[3]])
        }
        Some(_) => return Err(invalid("bbox must be [west, south, east, north]")),
    };
    let compression = if streaming {
        match o.compression.as_deref().unwrap_or("none") {
            "none" => Compression::None,
            "gzip" => Compression::Gzip,
            "deflate" => Compression::Deflate,
            "config" => config_compression(&config),
            other => {
                return Err(invalid(format!(
                    "unknown compression {other:?} (none, gzip, deflate, config)"
                )))
            }
        }
    } else {
        config_compression(&config)
    };
    Ok(Prepared {
        opts: TileOptions {
            threads: o.threads.unwrap_or(0) as usize,
            bbox,
        },
        name: o.process_name.clone().unwrap_or_else(|| "process.lua".into()),
        profile: o.process_source.clone(),
        config,
        compression,
    })
}

/// Tiles, packed: one buffer and offsets.
#[derive(Default)]
pub(crate) struct TileBatch {
    z: Vec<u8>,
    x: Vec<u32>,
    y: Vec<u32>,
    offsets: Vec<u32>,
    data: Vec<u8>,
}

impl TileBatch {
    fn len(&self) -> usize {
        self.z.len()
    }

    fn push(&mut self, z: u8, x: u32, y: u32, data: &[u8]) {
        if self.offsets.is_empty() {
            self.offsets.push(0);
        }
        self.z.push(z);
        self.x.push(x);
        self.y.push(y);
        self.data.extend_from_slice(data);
        self.offsets.push(self.data.len() as u32);
    }

    pub(crate) fn into_js(self, env: &Env) -> Result<JsObject> {
        let mut o = env.create_object()?;
        o.set("kind", "tiles")?;
        o.set("z", Uint8Array::new(self.z))?;
        o.set("x", Uint32Array::new(self.x))?;
        o.set("y", Uint32Array::new(self.y))?;
        o.set("offsets", Uint32Array::new(self.offsets))?;
        o.set("data", Buffer::from(self.data))?;
        Ok(o)
    }
}

/// Batches flush at this many bytes too.
const MAX_BATCH_BYTES: usize = 16 << 20;

fn progress_due(last: &mut Option<(Instant, String)>, key: String, finished: bool) -> bool {
    let now = Instant::now();
    let due = match last {
        None => true,
        Some((t, k)) => *k != key || finished || now - *t >= Duration::from_millis(200),
    };
    if due {
        *last = Some((now, key));
    }
    due
}

fn progress_key(p: &Progress) -> (String, bool) {
    match p {
        Progress::Read {
            phase,
            blocks_done,
            blocks_total,
        } => (phase.as_str().to_string(), blocks_done == blocks_total),
        Progress::Tiles { zoom, done, total } => (format!("z{zoom}"), done == total),
    }
}

pub(crate) fn spawn_tiles(
    file: PbfFile,
    p: Prepared,
    batch_size: usize,
    read_ahead: u32,
    cb: Callback,
) -> NativeStream {
    let credits = Arc::new(Credits::default());
    credits.grant(read_ahead.max(1));
    let handle = NativeStream {
        credits: credits.clone(),
    };
    thread::Builder::new()
        .name("tilemaker-tiles".into())
        .spawn(move || {
            let mut batch = TileBatch::default();
            let mut last: Option<(Instant, String)> = None;
            let flush = |batch: &mut TileBatch| -> Flow {
                if batch.len() == 0 {
                    return Flow::Continue;
                }
                if !credits.acquire() {
                    return Flow::Stop;
                }
                cb.call(
                    StreamEvent::Tiles(std::mem::take(batch)),
                    ThreadsafeFunctionCallMode::Blocking,
                );
                Flow::Continue
            };
            let result = generate(&file, &p.config, &p.profile, &p.name, &p.opts, |e| {
                if credits.is_closed() {
                    return Flow::Stop;
                }
                match e {
                    Event::Tile(t) => {
                        let data = tilemaker_output::compress(&t.data, p.compression);
                        batch.push(t.z, t.x, t.y, &data);
                        if batch.len() >= batch_size || batch.data.len() >= MAX_BATCH_BYTES {
                            return flush(&mut batch);
                        }
                        Flow::Continue
                    }
                    Event::Progress(pr) => {
                        let (key, finished) = progress_key(&pr);
                        if progress_due(&mut last, key, finished) {
                            cb.call(StreamEvent::TileProgress(pr), ThreadsafeFunctionCallMode::NonBlocking);
                        }
                        Flow::Continue
                    }
                }
            });
            let event = match result {
                Ok((mut stats, meta)) => {
                    if flush(&mut batch) == Flow::Stop {
                        stats.stopped = true;
                    }
                    end_event(&stats, Some(&meta))
                }
                Err(e) => StreamEvent::Error(e.to_string()),
            };
            cb.call(event, ThreadsafeFunctionCallMode::Blocking);
        })
        .expect("spawn tiles thread");
    handle
}

pub(crate) fn spawn_write(file: PbfFile, p: Prepared, output: String, cb: Callback) -> NativeStream {
    let credits = Arc::new(Credits::default());
    let handle = NativeStream {
        credits: credits.clone(),
    };
    thread::Builder::new()
        .name("tilemaker-write".into())
        .spawn(move || {
            let run = || -> std::result::Result<StreamEvent, String> {
                let path = std::path::Path::new(&output);
                let is_file = matches!(
                    path.extension().and_then(|e| e.to_str()),
                    Some("mbtiles" | "sqlite" | "pmtiles")
                );
                if is_file && path.exists() {
                    std::fs::remove_file(path).map_err(|e| format!("removing {output}: {e}"))?;
                }
                let mut sink = tilemaker_output::open_sink(path, p.compression, false).map_err(|e| e.to_string())?;
                let mut last: Option<(Instant, String)> = None;
                let (stats, meta) = write_tiles(&file, &p.config, &p.profile, &p.name, &p.opts, sink.as_mut(), |pr| {
                    if credits.is_closed() {
                        return Flow::Stop;
                    }
                    let (key, finished) = progress_key(&pr);
                    if progress_due(&mut last, key, finished) {
                        cb.call(StreamEvent::TileProgress(pr), ThreadsafeFunctionCallMode::NonBlocking);
                    }
                    Flow::Continue
                })
                .map_err(|e| e.to_string())?;
                Ok(end_event(&stats, Some(&meta)))
            };
            let event = run().unwrap_or_else(StreamEvent::Error);
            cb.call(event, ThreadsafeFunctionCallMode::Blocking);
        })
        .expect("spawn write thread");
    handle
}

fn end_event(stats: &tilemaker_core::TileStats, meta: Option<&tilemaker_output::TilesetMetadata>) -> StreamEvent {
    let mut v = serde_json::to_value(stats).unwrap_or_default();
    if let (Some(m), Some(obj)) = (meta, v.as_object_mut()) {
        obj.insert(
            "metadata".into(),
            serde_json::json!({
                "name": m.name,
                "version": m.version,
                "description": m.description,
                "minzoom": m.minzoom,
                "maxzoom": m.maxzoom,
                "bounds": m.bounds,
                "center": m.center.map(|c| vec![c.0, c.1, c.2 as f64]),
                "vector_layers": serde_json::from_str::<serde_json::Value>(&m.vector_layers_json).unwrap_or_default(),
            }),
        );
    }
    StreamEvent::End(v)
}
