//! Extracts written as PBF, as `osmium extract` and `osmium tags-filter`
//! make them, in one run.
//!
//! * A **region** (a box or polygons) keeps what `osmium extract` keeps
//!   with its default `complete_ways` strategy: the nodes inside, every way
//!   with a node inside together with all of its nodes, every relation with
//!   a node inside or one of those ways as a member, and the relations
//!   those are members of, up the hierarchy (relations are not completed).
//! * **Filters** then keep, of that, what `osmium tags-filter` keeps: the
//!   matching elements, the nodes of matching ways, and the members of
//!   matching relations — recursively through member relations, with
//!   their ways' nodes — unless references are omitted (`-R`).
//!
//! Without a region the whole file is the input; without filters every
//! element in the region is kept. Elements keep their tags and metadata;
//! the output is sorted when the input is.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use tilemaker_geom::assemble::RingIndex;
use tilemaker_geom::{Coord, MultiPolygon};
use tilemaker_pbf::write::{BlockBuilder, ParallelWriter, WriterOptions};
use tilemaker_pbf::{DecodeOptions, ElementType, HeaderBbox, PbfFile, PrimitiveBlock};
use tilemaker_store::{IdSet, IdSetBuilder};

use crate::engine::{is_area_relation, threads, Event, Flow, Phase, Progress, Runner};
use crate::filter::FilterSet;
use crate::ExportError;

/// The area an extract keeps.
#[derive(Clone, Debug, PartialEq)]
pub enum Region {
    /// `[west, south, east, north]`.
    Bbox([f64; 4]),
    /// Polygons in degrees (lon, lat): a point is inside when it is inside
    /// an exterior and outside that polygon's holes (or on a ring).
    Polygons(MultiPolygon),
}

impl Region {
    /// Polygons from GeoJSON: a `Polygon` or `MultiPolygon` geometry, a
    /// `Feature` with one, or a `FeatureCollection` of them.
    pub fn from_geojson(v: &serde_json::Value) -> Result<Self, ExportError> {
        let mut polys = Vec::new();
        collect_geojson(v, &mut polys)?;
        if polys.is_empty() {
            return Err(ExportError::Invalid("the GeoJSON region has no polygons".into()));
        }
        Ok(Region::Polygons(MultiPolygon::new(polys)))
    }

    /// Polygons from an osmosis `.poly` file (sections of `lon lat` lines,
    /// `!`-named sections are holes in the polygon before them).
    pub fn from_poly(text: &str) -> Result<Self, ExportError> {
        let bad = |m: &str| ExportError::Invalid(format!("bad .poly file: {m}"));
        let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
        lines.next().ok_or_else(|| bad("empty"))?;
        let mut polys: Vec<tilemaker_geom::Polygon> = Vec::new();
        loop {
            let Some(name) = lines.next() else {
                return Err(bad("missing END"));
            };
            if name == "END" {
                break;
            }
            let hole = name.starts_with('!');
            let mut ring = Vec::new();
            loop {
                let line = lines.next().ok_or_else(|| bad("section without END"))?;
                if line == "END" {
                    break;
                }
                let mut it = line.split_whitespace().map(str::parse::<f64>);
                match (it.next(), it.next()) {
                    (Some(Ok(x)), Some(Ok(y))) => ring.push(Coord { x, y }),
                    _ => return Err(bad(line)),
                }
            }
            let ring = tilemaker_geom::LineString::new(ring);
            if hole {
                let p = polys.last_mut().ok_or_else(|| bad("a hole before any polygon"))?;
                p.interiors_push(ring);
            } else {
                polys.push(tilemaker_geom::Polygon::new(ring, vec![]));
            }
        }
        if polys.is_empty() {
            return Err(bad("no polygons"));
        }
        Ok(Region::Polygons(MultiPolygon::new(polys)))
    }

    /// The region's bounding box.
    pub fn bbox(&self) -> [f64; 4] {
        match self {
            Region::Bbox(b) => *b,
            Region::Polygons(mp) => {
                let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
                for p in &mp.0 {
                    for c in &p.exterior().0 {
                        b = [b[0].min(c.x), b[1].min(c.y), b[2].max(c.x), b[3].max(c.y)];
                    }
                }
                b
            }
        }
    }
}

fn collect_geojson(v: &serde_json::Value, out: &mut Vec<tilemaker_geom::Polygon>) -> Result<(), ExportError> {
    let bad = |m: &str| ExportError::Invalid(format!("bad GeoJSON region: {m}"));
    let ring = |r: &serde_json::Value| -> Result<tilemaker_geom::LineString, ExportError> {
        let pts = r.as_array().ok_or_else(|| bad("a ring is not an array"))?;
        pts.iter()
            .map(
                |p| match (p.get(0).and_then(|x| x.as_f64()), p.get(1).and_then(|y| y.as_f64())) {
                    (Some(x), Some(y)) => Ok(Coord { x, y }),
                    _ => Err(bad("a position is not [lon, lat]")),
                },
            )
            .collect::<Result<Vec<_>, _>>()
            .map(tilemaker_geom::LineString::new)
    };
    let polygon = |p: &serde_json::Value| -> Result<tilemaker_geom::Polygon, ExportError> {
        let rings = p.as_array().ok_or_else(|| bad("a polygon is not an array"))?;
        let mut rings = rings.iter().map(ring);
        let ext = rings.next().ok_or_else(|| bad("a polygon without rings"))??;
        Ok(tilemaker_geom::Polygon::new(ext, rings.collect::<Result<_, _>>()?))
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("FeatureCollection") => {
            for f in v["features"]
                .as_array()
                .ok_or_else(|| bad("features is not an array"))?
            {
                collect_geojson(f, out)?;
            }
        }
        Some("Feature") => collect_geojson(&v["geometry"], out)?,
        Some("Polygon") => out.push(polygon(&v["coordinates"])?),
        Some("MultiPolygon") => {
            for p in v["coordinates"]
                .as_array()
                .ok_or_else(|| bad("coordinates is not an array"))?
            {
                out.push(polygon(p)?);
            }
        }
        Some(other) => return Err(bad(&format!("{other} is not a polygon"))),
        None => return Err(bad("no type")),
    }
    Ok(())
}

/// Point-in-region tests, prepared.
enum Inside {
    Bbox([i64; 4]),
    Polygons(Vec<([f64; 4], Vec<RingIndex>)>),
}

impl Inside {
    fn new(r: &Region) -> Self {
        match r {
            Region::Bbox(b) => Inside::Bbox(b.map(|v| (v * 1e7).round() as i64)),
            Region::Polygons(mp) => Inside::Polygons(
                mp.0.iter()
                    .map(|p| {
                        let b = p.exterior().0.iter().fold(
                            [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY],
                            |b, c| [b[0].min(c.x), b[1].min(c.y), b[2].max(c.x), b[3].max(c.y)],
                        );
                        let rings = std::iter::once(p.exterior())
                            .chain(p.interiors())
                            .map(|r| RingIndex::new(&r.0))
                            .collect();
                        (b, rings)
                    })
                    .collect(),
            ),
        }
    }

    /// Whether a location (1e-7 degrees) is inside.
    fn contains(&self, lat: i32, lon: i32) -> bool {
        match self {
            Inside::Bbox(b) => {
                let (x, y) = (lon as i64, lat as i64);
                x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3]
            }
            Inside::Polygons(polys) => {
                let p = Coord {
                    x: lon as f64 / 1e7,
                    y: lat as f64 / 1e7,
                };
                polys.iter().any(|(b, rings)| {
                    if p.x < b[0] || p.x > b[2] || p.y < b[1] || p.y > b[3] {
                        return false;
                    }
                    match rings[0].contains(p) {
                        None => true,
                        Some(false) => false,
                        Some(true) => rings[1..].iter().all(|h| h.contains(p) != Some(true)),
                    }
                })
            }
        }
    }
}

/// Options for [`extract`].
#[derive(Clone, Debug)]
pub struct ExtractOptions {
    /// `osmium tags-filter` expressions; `None` keeps every element (of
    /// the region).
    pub filters: Option<FilterSet>,
    /// The region; `None` for the whole file.
    pub region: Option<Region>,
    /// Keep only matching elements, not the nodes and members they
    /// reference (`osmium tags-filter -R`).
    pub omit_referenced: bool,
    /// Keep metadata (version, timestamp, changeset, uid, user).
    pub metadata: bool,
    /// Worker threads (0: all cores).
    pub threads: usize,
    /// The writing program recorded in the header.
    pub writing_program: Option<String>,
}

impl Default for ExtractOptions {
    fn default() -> Self {
        Self {
            filters: None,
            region: None,
            omit_referenced: false,
            metadata: true,
            threads: 0,
            writing_program: None,
        }
    }
}

/// Counters from an extract.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractStats {
    /// Nodes written.
    pub nodes: u64,
    /// Ways written.
    pub ways: u64,
    /// Relations written.
    pub relations: u64,
    /// Data blocks written.
    pub blocks: u64,
    /// Bytes written.
    pub bytes: u64,
    /// Whether it was stopped early (the output is then incomplete).
    pub stopped: bool,
    /// Wall-clock milliseconds.
    pub elapsed_ms: u64,
}

fn freeze(b: &Mutex<Option<IdSetBuilder>>) -> IdSet {
    b.lock().unwrap().take().map(IdSetBuilder::freeze).unwrap_or_default()
}

fn only(kind: ElementType, metadata: bool) -> DecodeOptions {
    DecodeOptions {
        nodes: kind == ElementType::Node,
        ways: kind == ElementType::Way,
        relations: kind == ElementType::Relation,
        metadata,
    }
}

/// `(child, parent)` edges between relations.
type Edges = Mutex<Vec<(i64, i64)>>;

/// Relations reachable from `start` along `edges` (`(from, to)`, sorted),
/// staying within `allowed`.
fn closure(start: Vec<i64>, edges: &[(i64, i64)], allowed: Option<&IdSet>) -> Vec<i64> {
    let mut seen: std::collections::HashSet<i64> = start.iter().copied().collect();
    let mut todo = start;
    while let Some(r) = todo.pop() {
        let from = edges.partition_point(|e| e.0 < r);
        for &(_, next) in edges[from..].iter().take_while(|e| e.0 == r) {
            if allowed.is_none_or(|a| a.contains(next)) && seen.insert(next) {
                todo.push(next);
            }
        }
    }
    let mut v: Vec<i64> = seen.into_iter().collect();
    v.sort_unstable();
    v
}

fn set_of(ids: &[i64]) -> IdSet {
    let b = IdSetBuilder::new();
    b.insert_all(ids);
    b.freeze()
}

/// Write an extract of `file` to `out` (see the module docs). `progress`
/// sees each pass's blocks and can stop the extract.
pub fn extract<W: Write + Send + 'static>(
    file: &PbfFile,
    opts: &ExtractOptions,
    out: W,
    mut progress: impl FnMut(Progress) -> Flow,
) -> Result<(W, ExtractStats), ExportError> {
    let started = Instant::now();
    let nthreads = threads(opts.threads);
    let mut runner = Runner {
        file,
        threads: nthreads,
        layout: file.layout()?,
        stopped: false,
        emitted: 0,
    };
    let mut sink = |e: Event<()>| match e {
        Event::Progress(p) => progress(p),
        Event::Batch(_) => Flow::Continue,
    };

    // 1. The region: nodes inside, ways touching it (with their nodes),
    //    relations with a member there, and their parents.
    struct RegionSets {
        nodes: Arc<IdSet>,
        ways: Arc<IdSet>,
        relations: Arc<IdSet>,
    }
    let mut region = None;
    if let Some(r) = &opts.region {
        let inside = Arc::new(Inside::new(r));
        let n_in = Arc::new(Mutex::new(Some(IdSetBuilder::new())));
        // Inside, or a node of a way that is: the nodes the region keeps.
        let n_all = Arc::new(Mutex::new(Some(IdSetBuilder::new())));
        {
            let (inside, n_in, n_all) = (inside.clone(), n_in.clone(), n_all.clone());
            runner.pass(Phase::Nodes, only(ElementType::Node, false), &mut sink, move |_, b| {
                let ids: Vec<i64> = b
                    .nodes()
                    .filter(|n| inside.contains(n.lat_e7(), n.lon_e7()))
                    .map(|n| n.id())
                    .collect();
                n_in.lock().unwrap().as_ref().unwrap().insert_all(&ids);
                n_all.lock().unwrap().as_ref().unwrap().insert_all(&ids);
                Ok(Vec::<()>::new())
            })?;
        }
        let n_in = Arc::new(freeze(&n_in));
        let w_reg = Arc::new(Mutex::new(Some(IdSetBuilder::new())));
        if !runner.stopped {
            let (n_in, w_reg, n_all) = (n_in.clone(), w_reg.clone(), n_all.clone());
            runner.pass(
                Phase::ScanWays,
                only(ElementType::Way, false),
                &mut sink,
                move |_, b| {
                    let ways = w_reg.lock().unwrap();
                    let nodes = n_all.lock().unwrap();
                    for w in b.ways() {
                        if w.refs().iter().any(|&n| n_in.contains(n)) {
                            ways.as_ref().unwrap().insert(w.id());
                            nodes.as_ref().unwrap().insert_all(w.refs());
                        }
                    }
                    Ok(Vec::<()>::new())
                },
            )?;
        }
        let w_reg = Arc::new(freeze(&w_reg));
        let hits: Arc<Mutex<Vec<i64>>> = Arc::default();
        let up: Arc<Edges> = Arc::default();
        if !runner.stopped {
            let (n_in, w_reg, hits, up) = (n_in.clone(), w_reg.clone(), hits.clone(), up.clone());
            runner.pass(
                Phase::ScanRelations,
                only(ElementType::Relation, false),
                &mut sink,
                move |_, b| {
                    let (mut found, mut edges) = (Vec::new(), Vec::new());
                    for r in b.relations() {
                        let mut hit = false;
                        for m in r.members() {
                            match m.kind {
                                ElementType::Node => hit |= n_in.contains(m.id),
                                ElementType::Way => hit |= w_reg.contains(m.id),
                                ElementType::Relation => edges.push((m.id, r.id())),
                            }
                        }
                        if hit {
                            found.push(r.id());
                        }
                    }
                    hits.lock().unwrap().extend(found);
                    up.lock().unwrap().extend(edges);
                    Ok(Vec::<()>::new())
                },
            )?;
        }
        let mut up = std::mem::take(&mut *up.lock().unwrap());
        up.sort_unstable();
        let rels = closure(std::mem::take(&mut *hits.lock().unwrap()), &up, None);
        region = Some(RegionSets {
            nodes: Arc::new(freeze(&n_all)),
            ways: w_reg,
            relations: Arc::new(set_of(&rels)),
        });
    }
    let region = Arc::new(region);

    // 2. The filters: matching relations and everything they reach, the
    //    members of those, and the nodes of matching and member ways.
    struct FilterSets {
        relations: Arc<IdSet>,
        ways: Arc<IdSet>,
        nodes: IdSet,
    }
    let mut picked = None;
    if let Some(filters) = &opts.filters {
        let filters = Arc::new(filters.clone());
        let matched: Arc<Mutex<Vec<i64>>> = Arc::default();
        let down: Arc<Edges> = Arc::default();
        if !runner.stopped && filters.may_select(ElementType::Relation) {
            let (filters, region, matched, down) = (filters.clone(), region.clone(), matched.clone(), down.clone());
            runner.pass(
                Phase::ScanRelations,
                only(ElementType::Relation, false),
                &mut sink,
                move |_, b| {
                    let bf = filters.for_block(b);
                    let (mut found, mut edges) = (Vec::new(), Vec::new());
                    for r in b.relations() {
                        if region.as_ref().as_ref().is_some_and(|s| !s.relations.contains(r.id())) {
                            continue;
                        }
                        let tags = r.tags();
                        if bf.matches(ElementType::Relation, is_area_relation(&tags), &tags) {
                            found.push(r.id());
                        }
                        for m in r.members() {
                            if m.kind == ElementType::Relation {
                                edges.push((r.id(), m.id));
                            }
                        }
                    }
                    matched.lock().unwrap().extend(found);
                    down.lock().unwrap().extend(edges);
                    Ok(Vec::<()>::new())
                },
            )?;
        }
        let mut down = std::mem::take(&mut *down.lock().unwrap());
        down.sort_unstable();
        let matched = std::mem::take(&mut *matched.lock().unwrap());
        let rels = if opts.omit_referenced {
            let mut m = matched;
            m.sort_unstable();
            m
        } else {
            let allowed = region.as_ref().as_ref().map(|r| &*r.relations);
            closure(matched, &down, allowed)
        };
        drop(down);
        let rel_set = Arc::new(set_of(&rels));
        let w_ref = Arc::new(Mutex::new(Some(IdSetBuilder::new())));
        let n_ref = Arc::new(Mutex::new(Some(IdSetBuilder::new())));
        if !opts.omit_referenced && !rels.is_empty() && !runner.stopped {
            let (region, rel_set, w_ref, n_ref) = (region.clone(), rel_set.clone(), w_ref.clone(), n_ref.clone());
            runner.pass(
                Phase::ScanRelations,
                only(ElementType::Relation, false),
                &mut sink,
                move |_, b| {
                    let (mut ways, mut nodes) = (Vec::new(), Vec::new());
                    for r in b.relations() {
                        if !rel_set.contains(r.id()) {
                            continue;
                        }
                        for m in r.members() {
                            let s = region.as_ref().as_ref();
                            match m.kind {
                                ElementType::Way if s.is_none_or(|s| s.ways.contains(m.id)) => ways.push(m.id),
                                ElementType::Node if s.is_none_or(|s| s.nodes.contains(m.id)) => nodes.push(m.id),
                                _ => {}
                            }
                        }
                    }
                    w_ref.lock().unwrap().as_ref().unwrap().insert_all(&ways);
                    n_ref.lock().unwrap().as_ref().unwrap().insert_all(&nodes);
                    Ok(Vec::<()>::new())
                },
            )?;
        }
        let w_ref = Arc::new(freeze(&w_ref));
        let wants_ways = filters.may_select(ElementType::Way) || !w_ref.is_empty();
        if !opts.omit_referenced && wants_ways && !runner.stopped {
            let (filters, region, w_ref, n_ref) = (filters.clone(), region.clone(), w_ref.clone(), n_ref.clone());
            runner.pass(
                Phase::ScanWays,
                only(ElementType::Way, false),
                &mut sink,
                move |_, b| {
                    let bf = filters.for_block(b);
                    let nodes = n_ref.lock().unwrap();
                    let nodes = nodes.as_ref().unwrap();
                    for w in b.ways() {
                        if region.as_ref().as_ref().is_some_and(|s| !s.ways.contains(w.id())) {
                            continue;
                        }
                        if w_ref.contains(w.id()) || way_matches(&bf, &w) {
                            match region.as_ref() {
                                Some(s) => {
                                    let refs: Vec<i64> =
                                        w.refs().iter().copied().filter(|&n| s.nodes.contains(n)).collect();
                                    nodes.insert_all(&refs);
                                }
                                None => nodes.insert_all(w.refs()),
                            }
                        }
                    }
                    Ok(Vec::<()>::new())
                },
            )?;
        }
        picked = Some(FilterSets {
            relations: rel_set,
            ways: w_ref,
            nodes: freeze(&n_ref),
        });
    }
    let picked = Arc::new(picked);

    // 3. Write: every block, keeping what was selected.
    let header = file.header();
    let wopts = WriterOptions {
        bbox: opts
            .region
            .as_ref()
            .map(|r| {
                let b = r.bbox();
                HeaderBbox {
                    min_lon: b[0],
                    min_lat: b[1],
                    max_lon: b[2],
                    max_lat: b[3],
                }
            })
            .or(header.bbox),
        sorted: header.is_sorted(),
        locations_on_ways: header.has_locations_on_ways(),
        writing_program: Some(
            opts.writing_program
                .clone()
                .unwrap_or_else(|| format!("tilemaker-rs {}", env!("CARGO_PKG_VERSION"))),
        ),
        uncompressed: false,
    };
    let mut writer = ParallelWriter::new(out, wopts, nthreads)?;
    let mut stats = ExtractStats::default();
    if !runner.stopped {
        let filters = Arc::new(opts.filters.clone());
        let decode = DecodeOptions {
            metadata: opts.metadata,
            ..DecodeOptions::ALL
        };
        let total = file.blobs().len();
        let metadata = opts.metadata;
        let (region, picked) = (region.clone(), picked.clone());
        let blocks = file.map_blocks(0..total, decode, nthreads, move |_, b| {
            let region = region.as_ref().as_ref().map(|s| (&*s.nodes, &*s.ways, &*s.relations));
            let picked = picked.as_ref().as_ref().map(|p| (&p.nodes, &*p.ways, &*p.relations));
            Ok(select(&b, filters.as_ref().as_ref(), region, picked, metadata))
        });
        for (done, b) in blocks.enumerate() {
            writer.push(b?)?;
            let p = Progress {
                phase: Phase::Write,
                blocks_done: done + 1,
                blocks_total: total,
            };
            if progress(p) == Flow::Stop {
                runner.stopped = true;
                break;
            }
        }
    }
    let (out, w) = writer.finish()?;
    stats.nodes = w.nodes;
    stats.ways = w.ways;
    stats.relations = w.relations;
    stats.blocks = w.blocks;
    stats.bytes = w.bytes;
    stats.stopped = runner.stopped;
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok((out, stats))
}

fn way_matches(bf: &crate::filter::BlockFilter<'_>, w: &tilemaker_pbf::Way<'_>) -> bool {
    let tags = w.tags();
    let r = w.refs();
    let closed = r.len() >= 4 && r.first() == r.last();
    bf.matches(ElementType::Way, closed && crate::area::is_area_by_tags(&tags), &tags)
}

type Sets<'a> = (&'a IdSet, &'a IdSet, &'a IdSet);

/// One block's selected elements.
fn select(
    b: &PrimitiveBlock,
    filters: Option<&FilterSet>,
    region: Option<Sets<'_>>,
    picked: Option<Sets<'_>>,
    metadata: bool,
) -> BlockBuilder {
    let mut out = BlockBuilder::new();
    let bf = filters.map(|f| f.for_block(b));
    let info = |i: Option<tilemaker_pbf::InfoRef<'_>>| -> Option<(tilemaker_pbf::Info, String)> {
        if metadata {
            i.map(|i| (*i.info(), i.user().to_string()))
        } else {
            None
        }
    };
    for n in b.nodes() {
        if region.is_some_and(|r| !r.0.contains(n.id())) {
            continue;
        }
        let keep = match (&bf, picked) {
            (Some(bf), Some(p)) => {
                p.0.contains(n.id()) || (!n.tags().is_empty() && bf.matches(ElementType::Node, false, &n.tags()))
            }
            _ => true,
        };
        if keep {
            let i = info(n.info());
            out.node(
                n.id(),
                n.lat_e7(),
                n.lon_e7(),
                n.tags().iter(),
                i.as_ref().map(|(i, u)| (i, u.as_str())),
            );
        }
    }
    for w in b.ways() {
        if region.is_some_and(|r| !r.1.contains(w.id())) {
            continue;
        }
        let keep = match (&bf, picked) {
            (Some(bf), Some(p)) => p.1.contains(w.id()) || way_matches(bf, &w),
            _ => true,
        };
        if keep {
            let i = info(w.info());
            out.way(
                w.id(),
                w.refs(),
                w.locations(),
                w.tags().iter(),
                i.as_ref().map(|(i, u)| (i, u.as_str())),
            );
        }
    }
    for r in b.relations() {
        if region.is_some_and(|s| !s.2.contains(r.id())) {
            continue;
        }
        let keep = match picked {
            Some(p) => p.2.contains(r.id()),
            None => true,
        };
        if keep {
            let i = info(r.info());
            out.relation(
                r.id(),
                r.members().map(|m| (m.id, m.kind, m.role)),
                r.tags().iter(),
                i.as_ref().map(|(i, u)| (i, u.as_str())),
            );
        }
    }
    out
}
