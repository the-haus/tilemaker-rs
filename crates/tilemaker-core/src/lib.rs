//! Turn OpenStreetMap PBF files into Mapbox vector tiles with a tilemaker
//! Lua profile (`process.lua`) and JSON config (`config.json`).
//!
//! ```no_run
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! use tilemaker_core::{write_tiles, Flow, TileOptions};
//!
//! let file = tilemaker_pbf::PbfFile::open("monaco.osm.pbf")?;
//! let config = tilemaker_config::Config::from_path("config.json")?;
//! let profile = std::fs::read_to_string("process.lua")?;
//! let mut sink = tilemaker_output::open_sink("monaco.pmtiles".as_ref(), tilemaker_output::Compression::Gzip, false)?;
//! let (stats, _metadata) = write_tiles(&file, &config, &profile, "process.lua", &TileOptions::default(), sink.as_mut(), |_| Flow::Continue)?;
//! println!("{} tiles", stats.tiles);
//! # Ok(()) }
//! ```
//!
//! Not supported (yet): shapefile/GeoJSON layers (`source` in the config and
//! the spatial Lua queries), decluttering (`declutter_below`, `Score`), and
//! merging into existing MBTiles.

mod data;
mod read;
mod significant;
mod tile;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Instant;

use tilemaker_config::Config;
use tilemaker_lua::Shared;
use tilemaker_output::{TileSink, TilesetMetadata};
use tilemaker_pbf::PbfFile;

pub use read::ReadStats;
pub use significant::SignificantTags;

/// Errors from tile generation.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Reading the PBF file.
    #[error(transparent)]
    Pbf(#[from] tilemaker_pbf::Error),
    /// The Lua profile.
    #[error(transparent)]
    Lua(#[from] tilemaker_lua::ProfileError),
    /// A profile setting (e.g. `node_keys`).
    #[error("profile: {0}")]
    Profile(String),
    /// The location stores.
    #[error(transparent)]
    Store(#[from] tilemaker_store::StoreError),
    /// Writing tiles.
    #[error(transparent)]
    Output(#[from] tilemaker_output::OutputError),
    /// A config problem.
    #[error(transparent)]
    Config(#[from] tilemaker_config::ConfigError),
    /// Building the thread pool.
    #[error("thread pool: {0}")]
    Pool(String),
}

/// A pass over the OSM file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Relations through `relation_scan_function`.
    ScanRelations,
    /// Ways, to find the nodes they need (with `way_keys`).
    ScanWays,
    /// Nodes.
    Nodes,
    /// Ways.
    Ways,
    /// Relations.
    Relations,
}

impl Phase {
    /// camelCase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::ScanRelations => "scanRelations",
            Phase::ScanWays => "scanWays",
            Phase::Nodes => "nodes",
            Phase::Ways => "ways",
            Phase::Relations => "relations",
        }
    }
}

/// Progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    /// Reading the file.
    Read {
        /// Pass.
        phase: Phase,
        /// Blocks done.
        blocks_done: usize,
        /// Blocks in the pass.
        blocks_total: usize,
    },
    /// Writing tiles.
    Tiles {
        /// Zoom.
        zoom: u8,
        /// Tiles done at this zoom.
        done: usize,
        /// Tiles at this zoom.
        total: usize,
    },
}

/// Whether to keep going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Continue.
    Continue,
    /// Stop.
    Stop,
}

/// A finished tile (uncompressed MVT).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    /// Zoom.
    pub z: u8,
    /// Column.
    pub x: u32,
    /// Row (XYZ).
    pub y: u32,
    /// MVT bytes, compressed as [`TileOptions::compression`] says.
    pub data: Vec<u8>,
}

/// Options.
#[derive(Clone, Debug, Default)]
pub struct TileOptions {
    /// Worker threads (0: all cores).
    pub threads: usize,
    /// Only write tiles intersecting `[west, south, east, north]` (instead
    /// of the file header's box, as tilemaker's `--bbox`).
    pub bbox: Option<[f64; 4]>,
    /// Compress tiles as they are rendered, on the worker threads (default
    /// none). [`write_tiles`] sets this to the sink's compression.
    pub compression: tilemaker_output::Compression,
}

/// Counters from a run.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TileStats {
    /// Reading.
    pub read: ReadStats,
    /// Tiles written.
    pub tiles: u64,
    /// Tiles written per zoom (index = zoom).
    pub tiles_per_zoom: Vec<u64>,
    /// Uncompressed MVT bytes written.
    pub bytes: u64,
    /// Profile warnings (first 100).
    pub warnings: Vec<String>,
    /// Whether the run was stopped early.
    pub stopped: bool,
    /// Wall-clock milliseconds.
    pub elapsed_ms: u64,
}

/// What [`generate`] hands out.
#[derive(Debug)]
pub enum Event {
    /// A tile.
    Tile(Tile),
    /// Progress.
    Progress(Progress),
}

fn threads(n: usize) -> usize {
    if n == 0 {
        tilemaker_pbf::parallel::default_threads()
    } else {
        n
    }
}

/// Generate tiles, handing each to `sink` (zoom by zoom, then by x, y).
/// Returns the counters and the tileset metadata.
pub fn generate(
    file: &PbfFile,
    config: &Config,
    profile_source: &str,
    profile_name: &str,
    opts: &TileOptions,
    mut sink: impl FnMut(Event) -> Flow,
) -> Result<(TileStats, TilesetMetadata), CoreError> {
    let started = Instant::now();
    let config = config.clone();
    let mut clip = Vec::new();
    if let Some(b) = opts
        .bbox
        .or_else(|| file.header().bbox.map(|b| [b.min_lon, b.min_lat, b.max_lon, b.max_lat]))
    {
        clip.push(b);
    }
    if let Some(b) = config.bounding_box {
        clip.push(b);
    }
    let n = threads(opts.threads);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(n)
        .thread_name(|i| format!("tilemaker-{i}"))
        .build()
        .map_err(|e| CoreError::Pool(e.to_string()))?;
    let shared = Shared::new(&config);
    let stop = Arc::new(AtomicBool::new(false));
    let mut stats = TileStats {
        tiles_per_zoom: vec![0; config.maxzoom as usize + 1],
        ..Default::default()
    };
    let mut stopped = false;

    let (data, read_stats) = {
        let mut progress = |p: Progress| {
            let f = sink(Event::Progress(p));
            if f == Flow::Stop {
                stopped = true;
            }
            f
        };
        let mut reader = read::Reader {
            file,
            pool: &pool,
            threads: n,
            progress: &mut progress,
            stop: stop.clone(),
        };
        read::read(&mut reader, &config, profile_source, profile_name, shared.clone())?
    };
    stats.read = read_stats;

    let renderer = tile::Renderer {
        data: &data,
        config: &config,
        clip,
    };
    'zooms: for z in config.minzoom..=config.maxzoom {
        if stopped {
            break;
        }
        let tiles = renderer.zoom_tiles(z);
        let total = tiles.len();
        let mut done = 0;
        let mut iter = tiles.into_iter();
        loop {
            let chunk: Vec<_> = iter.by_ref().take(256).collect();
            if chunk.is_empty() {
                break;
            }
            let rendered: Vec<Option<(usize, Tile)>> = pool.install(|| {
                use rayon::prelude::*;
                chunk
                    .into_par_iter()
                    .map(|(t, objs)| {
                        renderer.render(z, t, objs).map(|data| {
                            let raw = data.len();
                            let data = match opts.compression {
                                tilemaker_output::Compression::None => data,
                                c => tilemaker_output::compress(&data, c),
                            };
                            (
                                raw,
                                Tile {
                                    z,
                                    x: t.x,
                                    y: t.y,
                                    data,
                                },
                            )
                        })
                    })
                    .collect()
            });
            done += rendered.len();
            for (raw, t) in rendered.into_iter().flatten() {
                stats.tiles += 1;
                stats.tiles_per_zoom[z as usize] += 1;
                stats.bytes += raw as u64;
                if sink(Event::Tile(t)) == Flow::Stop {
                    stopped = true;
                    break 'zooms;
                }
            }
            if sink(Event::Progress(Progress::Tiles { zoom: z, done, total })) == Flow::Stop {
                stopped = true;
                break 'zooms;
            }
        }
    }

    let mut meta = metadata(&config, file, &shared);
    if let Some(b) = opts.bbox {
        meta.bounds = Some(b);
    }
    stats.warnings = shared.warnings.lock().unwrap().clone();
    stats.stopped = stopped;
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok((stats, meta))
}

/// Generate tiles into a sink (MBTiles, PMTiles, a directory, …) and write
/// its metadata, which is returned too.
pub fn write_tiles(
    file: &PbfFile,
    config: &Config,
    profile_source: &str,
    profile_name: &str,
    opts: &TileOptions,
    sink: &mut dyn TileSink,
    mut progress: impl FnMut(Progress) -> Flow,
) -> Result<(TileStats, TilesetMetadata), CoreError> {
    let mut err = None;
    // Tiles are compressed on the workers, as the sink stores them.
    let opts = TileOptions {
        compression: sink.compression(),
        ..opts.clone()
    };
    let (stats, meta) = generate(file, config, profile_source, profile_name, &opts, |e| match e {
        Event::Tile(t) => match sink.write_compressed(t.z, t.x, t.y, &t.data) {
            Ok(()) => Flow::Continue,
            Err(e) => {
                err = Some(e);
                Flow::Stop
            }
        },
        Event::Progress(p) => progress(p),
    })?;
    if let Some(e) = err {
        return Err(e.into());
    }
    sink.finish(&meta)?;
    Ok((stats, meta))
}

/// The tileset metadata (`vector_layers` from the attributes the profile
/// wrote, bounds from the config, options or file header).
pub fn metadata(config: &Config, file: &PbfFile, shared: &Shared) -> TilesetMetadata {
    let fields = shared.fields.lock().unwrap();
    let mut layers = Vec::new();
    for (i, l) in config.layers.layers.iter().enumerate() {
        if l.write_to.is_some() {
            continue;
        }
        // Fields of this layer and of the layers that write into it.
        let mut f = serde_json::Map::new();
        for group in &config.layers.layer_order {
            if group[0] != i {
                continue;
            }
            for &member in group {
                for (k, t) in &fields[member] {
                    let ty = match t {
                        1 => "Number",
                        2 => "Boolean",
                        _ => "String",
                    };
                    f.entry(k.clone()).or_insert_with(|| ty.into());
                }
            }
        }
        layers.push(serde_json::json!({ "id": l.name, "fields": f, "minzoom": l.minzoom, "maxzoom": l.maxzoom }));
    }
    let bounds = config
        .bounding_box
        .or_else(|| file.header().bbox.map(|b| [b.min_lon, b.min_lat, b.max_lon, b.max_lat]));
    TilesetMetadata {
        name: config.name.clone(),
        version: config.version.clone(),
        description: config.description.clone(),
        minzoom: config.minzoom,
        maxzoom: config.maxzoom,
        bounds,
        center: config.default_view,
        vector_layers_json: serde_json::Value::Array(layers).to_string(),
        extra: config.file_metadata.clone(),
    }
}
