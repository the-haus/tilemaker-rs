//! `tilemaker`: OpenStreetMap PBF → vector tiles (MBTiles, PMTiles or a
//! directory), with the same arguments as the C++ tilemaker; plus
//! `tilemaker export` to stream filtered features as GeoJSON lines.

use std::io::{BufWriter, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use tilemaker_config::{Compress, Config};
use tilemaker_core::{write_tiles, Flow, Progress, TileOptions};
use tilemaker_output::Compression;
use tilemaker_pbf::PbfFile;

#[derive(Parser)]
#[command(
    name = "tilemaker",
    version,
    about = "Convert OpenStreetMap .pbf files into vector tiles",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[command(flatten)]
    tiles: TileArgs,
}

#[derive(Subcommand)]
enum Command {
    /// Stream filtered, assembled features as newline-delimited GeoJSON.
    Export(Box<ExportArgs>),
    /// Count features per zoom and layer in an .mbtiles or .pmtiles file.
    Inspect {
        /// The tileset.
        path: PathBuf,
        /// Print JSON.
        #[arg(long)]
        json: bool,
        /// One line per tile instead of per zoom.
        #[arg(long)]
        tiles: bool,
        /// With --tiles: also each layer's geometry size (u32 commands).
        #[arg(long)]
        detail: bool,
    },
}

#[derive(Args)]
struct TileArgs {
    /// Source .osm.pbf file.
    #[arg(long = "input", value_name = "FILE")]
    input_flag: Option<PathBuf>,
    /// Target directory or .mbtiles/.pmtiles file.
    #[arg(long = "output", value_name = "PATH")]
    output_flag: Option<PathBuf>,
    /// Source .osm.pbf file (positional).
    #[arg(value_name = "INPUT")]
    input: Option<PathBuf>,
    /// Target (positional).
    #[arg(value_name = "OUTPUT")]
    output: Option<PathBuf>,
    /// Config JSON file.
    #[arg(long, default_value = "config.json")]
    config: PathBuf,
    /// Tag-processing Lua file.
    #[arg(long, default_value = "process.lua")]
    process: PathBuf,
    /// Bounding box to use instead of the file's: minlon,minlat,maxlon,maxlat.
    #[arg(long, value_parser = parse_bbox)]
    bbox: Option<[f64; 4]>,
    /// Number of threads (0: all cores).
    #[arg(long, default_value_t = 0)]
    threads: usize,
    /// Suppress progress output.
    #[arg(long)]
    quiet: bool,
    /// Print profile warnings.
    #[arg(long)]
    verbose: bool,
    /// Merge with an existing .mbtiles (not supported; rejected).
    #[arg(long)]
    merge: bool,
}

#[derive(Args)]
struct ExportArgs {
    /// Source .osm.pbf file.
    input: PathBuf,
    /// Output .geojsonseq (default: stdout).
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// osmium-style tag filter (repeatable), e.g. n/amenity=cafe.
    #[arg(short, long = "filter")]
    filters: Vec<String>,
    /// Geometry types: point, linestring, polygon (comma-separated).
    #[arg(long, default_value = "point,linestring,polygon")]
    geometry_types: String,
    /// What closed ways become: auto, polygon, linestring, both.
    #[arg(long, default_value = "auto")]
    closed_ways: String,
    /// Turn non-area relations into multi-linestrings.
    #[arg(long)]
    relation_lines: bool,
    /// Only features intersecting minlon,minlat,maxlon,maxlat.
    #[arg(long, value_parser = parse_bbox)]
    bbox: Option<[f64; 4]>,
    /// Only these tag keys (repeatable, * wildcards).
    #[arg(long = "tag")]
    tags: Vec<String>,
    /// Keep node locations in this (sparse) file instead of memory.
    #[arg(long)]
    node_store_file: Option<PathBuf>,
    /// Number of threads (0: all cores).
    #[arg(long, default_value_t = 0)]
    threads: usize,
    /// Add metadata: @version, @timestamp, @changeset, @uid, @user.
    #[arg(long)]
    metadata: bool,
    /// Clip geometries to minlon,minlat,maxlon,maxlat.
    #[arg(long, value_parser = parse_bbox)]
    clip: Option<[f64; 4]>,
    /// Simplify geometries (Douglas–Peucker tolerance in degrees).
    #[arg(long, value_name = "DEGREES")]
    simplify: Option<f64>,
    /// Round coordinates to this many decimals.
    #[arg(long, value_name = "DECIMALS")]
    precision: Option<u8>,
    /// Add @area (m²).
    #[arg(long)]
    area: bool,
    /// Add @length (m; the perimeter of polygons).
    #[arg(long)]
    length: bool,
    /// Add @centroid.
    #[arg(long)]
    centroid: bool,
    /// Add @labelPoint (polygons' pole of inaccessibility, lines' midpoint).
    #[arg(long)]
    label_point: bool,
    /// Add each feature's bounding box (GeoJSON "bbox").
    #[arg(long)]
    feature_bbox: bool,
    /// Add @memberOf: the relations each element belongs to, of those these
    /// filters select (none: all relations).
    #[arg(long, num_args = 0.., value_name = "FILTER")]
    member_of: Option<Vec<String>>,
    /// Start each feature with a record separator (RFC 8142).
    #[arg(long)]
    record_separator: bool,
}

fn parse_bbox(s: &str) -> Result<[f64; 4], String> {
    let v: Vec<f64> = s
        .split(',')
        .map(|x| x.trim().parse::<f64>().map_err(|e| format!("{x:?}: {e}")))
        .collect::<Result<_, _>>()?;
    if v.len() != 4 {
        return Err("expected minlon,minlat,maxlon,maxlat".into());
    }
    Ok([v[0], v[1], v[2], v[3]])
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Export(a)) => export(*a),
        Some(Command::Inspect {
            path,
            json,
            tiles,
            detail,
        }) => inspect(&path, json, tiles, detail),
        None => tiles(cli.tiles),
    }
}

fn tiles(a: TileArgs) -> Result<()> {
    let input = a.input_flag.or(a.input).context("no input file (see --help)")?;
    let output = a
        .output_flag
        .or(a.output)
        .context("You must specify an output file or directory. Run with --help to find out more.")?;
    if a.merge {
        bail!("--merge is not supported by this version");
    }
    let config = Config::from_path(&a.config).with_context(|| format!("reading {}", a.config.display()))?;
    let profile = std::fs::read_to_string(&a.process).with_context(|| format!("reading {}", a.process.display()))?;
    let started = Instant::now();
    let file = PbfFile::open(&input).with_context(|| format!("opening {}", input.display()))?;
    for l in &config.layers.layers {
        if l.source.is_some() && !a.quiet {
            eprintln!(
                "warning: layer {} reads {} — shapefile/GeoJSON sources are not supported; it will be empty",
                l.name,
                l.source.as_deref().unwrap_or("")
            );
        }
    }
    let compression = match config.compress {
        Compress::Gzip => Compression::Gzip,
        Compress::Deflate => Compression::Deflate,
        Compress::None => Compression::None,
    };
    let is_file = matches!(
        output.extension().and_then(|e| e.to_str()),
        Some("mbtiles" | "sqlite" | "pmtiles")
    );
    if is_file && output.exists() {
        std::fs::remove_file(&output).with_context(|| format!("removing {}", output.display()))?;
    }
    let mut sink = tilemaker_output::open_sink(&output, compression, false)?;
    let tty = std::io::stderr().is_terminal();
    let quiet = a.quiet;
    let mut last = String::new();
    let (stats, _) = write_tiles(
        &file,
        &config,
        &profile,
        &a.process.display().to_string(),
        &TileOptions {
            threads: a.threads,
            bbox: a.bbox,
            ..Default::default()
        },
        sink.as_mut(),
        |p| {
            if quiet {
                return Flow::Continue;
            }
            let line = match p {
                Progress::Read {
                    phase,
                    blocks_done,
                    blocks_total,
                } => format!("{}: block {blocks_done}/{blocks_total}", phase.as_str()),
                Progress::Tiles { zoom, done, total } => format!("z{zoom}: {done}/{total} tiles"),
            };
            let key = line.split(':').next().unwrap_or("").to_string();
            if tty {
                eprint!("\r\x1b[K{line}");
            } else if key != last {
                eprintln!("{line}");
            }
            last = key;
            Flow::Continue
        },
    )?;
    if !quiet {
        if tty {
            eprintln!();
        }
        if a.verbose {
            for w in &stats.warnings {
                eprintln!("warning: {w}");
            }
        } else if !stats.warnings.is_empty() {
            eprintln!(
                "{} profile warnings (run with --verbose to see them)",
                stats.warnings.len()
            );
        }
        eprintln!(
            "Wrote {} tiles ({} objects) to {} in {:.1}s",
            stats.tiles,
            stats.read.objects,
            output.display(),
            started.elapsed().as_secs_f64()
        );
    }
    Ok(())
}

fn export(a: ExportArgs) -> Result<()> {
    use tilemaker_export::*;
    let file = PbfFile::open(&a.input).with_context(|| format!("opening {}", a.input.display()))?;
    let mut gt = GeometryTypes {
        points: false,
        lines: false,
        polygons: false,
    };
    for t in a.geometry_types.split(',') {
        match t.trim() {
            "point" => gt.points = true,
            "linestring" | "line" => gt.lines = true,
            "polygon" => gt.polygons = true,
            other => bail!("unknown geometry type {other:?}"),
        }
    }
    let closed_ways = match a.closed_ways.as_str() {
        "auto" => ClosedWays::Auto,
        "polygon" => ClosedWays::Polygon,
        "linestring" => ClosedWays::LineString,
        "both" => ClosedWays::Both,
        other => bail!("unknown --closed-ways {other:?}"),
    };
    let opts = FeatureOptions {
        common: CommonOptions {
            filters: FilterSet::parse(&a.filters)?,
            tags: if a.tags.is_empty() {
                TagSelection::All
            } else {
                TagSelection::keys(&a.tags)
            },
            metadata: a.metadata,
            bbox: a.bbox,
            threads: a.threads,
            node_storage: match a.node_store_file {
                Some(p) => NodeStorage::DenseFile(p),
                None => NodeStorage::Memory,
            },
        },
        geometry_types: gt,
        closed_ways,
        relation_lines: a.relation_lines,
        transform: Transform {
            clip: a.clip,
            simplify: a.simplify,
            precision: a.precision,
        },
        enrich: Enrich {
            area: a.area,
            length: a.length,
            centroid: a.centroid,
            label_point: a.label_point,
            bbox: a.feature_bbox,
        },
        member_of: a.member_of.as_deref().map(FilterSet::parse).transpose()?,
    };
    let out: Box<dyn Write> = match &a.output {
        Some(p) => Box::new(std::fs::File::create(p).with_context(|| format!("creating {}", p.display()))?),
        None => Box::new(std::io::stdout().lock()),
    };
    let mut out = BufWriter::with_capacity(1 << 20, out);
    let geojson = GeoJsonOptions {
        record_separator: a.record_separator,
    };
    let stats = match write_geojson_seq(&file, &opts, geojson, &mut out, |_| Flow::Continue) {
        Err(ExportError::Io(e)) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
        r => r?,
    };
    eprintln!("{}", serde_json::to_string(&stats)?);
    Ok(())
}

/// Per zoom: tiles, bytes, and per layer the number of features.
fn inspect(path: &std::path::Path, json: bool, per_tile: bool, detail: bool) -> Result<()> {
    use std::collections::BTreeMap;
    let mut tiles: Vec<(u8, u32, u32, Vec<u8>)> = Vec::new();
    match path.extension().and_then(|e| e.to_str()) {
        Some("pmtiles") => {
            let r = tilemaker_output::pmtiles::reader::PmtilesReader::open(path)?;
            for e in r.entries()? {
                for k in 0..e.run_length.max(1) as u64 {
                    let (z, x, y) = tilemaker_output::pmtiles::tileid_to_zxy(e.tile_id + k);
                    if let Some(d) = r.get_tile_decompressed(z, x, y)? {
                        tiles.push((z, x, y, d));
                    }
                }
            }
        }
        _ => {
            let db = rusqlite::Connection::open(path)?;
            let mut st = db.prepare("SELECT zoom_level, tile_column, tile_row, tile_data FROM tiles")?;
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, u8>(0)?,
                    r.get::<_, u32>(1)?,
                    r.get::<_, u32>(2)?,
                    r.get::<_, Vec<u8>>(3)?,
                ))
            })?;
            for row in rows {
                let (z, x, tms_y, d) = row?;
                tiles.push((z, x, (1u32 << z) - 1 - tms_y, tilemaker_output::decompress(&d)?));
            }
        }
    }
    tiles.sort_by_key(|t| (t.0, t.1, t.2));
    if per_tile {
        for (z, x, y, data) in &tiles {
            let t = tilemaker_mvt::decode(data).map_err(|e| anyhow::anyhow!("bad tile at z{z}: {e}"))?;
            let ls: Vec<String> = t
                .layers
                .iter()
                .map(|l| {
                    if detail {
                        let g: usize = l.features.iter().map(|f| f.geometry.len()).sum();
                        format!("{}={}/{g}", l.name, l.features.len())
                    } else {
                        format!("{}={}", l.name, l.features.len())
                    }
                })
                .collect();
            println!("{z}/{x}/{y}: {}", ls.join(" "));
        }
        return Ok(());
    }
    let mut per_zoom: BTreeMap<u8, (u64, u64, BTreeMap<String, u64>)> = BTreeMap::new();
    for (z, _, _, data) in &tiles {
        let t = tilemaker_mvt::decode(data).map_err(|e| anyhow::anyhow!("bad tile at z{z}: {e}"))?;
        let entry = per_zoom.entry(*z).or_default();
        entry.0 += 1;
        entry.1 += data.len() as u64;
        for l in t.layers {
            *entry.2.entry(l.name).or_default() += l.features.len() as u64;
        }
    }
    if json {
        let v: serde_json::Value = per_zoom
            .iter()
            .map(|(z, (n, b, layers))| {
                (
                    z.to_string(),
                    serde_json::json!({ "tiles": n, "bytes": b, "features": layers }),
                )
            })
            .collect::<serde_json::Map<_, _>>()
            .into();
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        for (z, (n, b, layers)) in &per_zoom {
            let ls: Vec<String> = layers.iter().map(|(k, v)| format!("{k}={v}")).collect();
            println!("z{z}: {n} tiles, {b} bytes: {}", ls.join(" "));
        }
    }
    Ok(())
}
