//! The passes over a PBF file.
//!
//! Features need geometry, and geometry needs locations that live in other
//! parts of the file (sorted files hold nodes, then ways, then relations).
//! To keep memory proportional to what is selected rather than to the file:
//!
//! 1. **Relations scan**: selected area/line relations mark their member ways.
//! 2. **Ways scan**: selected ways and marked ways mark their nodes.
//! 3. **Nodes**: selected nodes become points; marked nodes' locations are
//!    stored (compressed).
//! 4. **Ways**: selected ways become lines/polygons; marked ways'
//!    geometries are stored (compressed).
//! 5. **Relations**: selected relations are assembled from stored ways.
//!
//! Passes that cannot select anything are skipped, and each pass decodes
//! only the element type it needs, on all cores, in file order. Output is
//! handed to a sink block by block; the sink can stop the export early.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use tilemaker_geom::assemble::{assemble_area, merge_ways};
use tilemaker_geom::LatpLon;
use tilemaker_pbf::{DecodeOptions, ElementType, Layout, PbfFile, PrimitiveBlock, Relation, Tags, Way};
use tilemaker_store::{
    IdSet, IdSetBuilder, Location, NodeStore, NodeStoreBuilder, RefBatch, RefStore, RefStoreBuilder, WayBatch,
    WayStore, WayStoreBuilder,
};

use crate::area::ClosedWays;
use crate::filter::{BlockFilter, FilterSet};
use crate::record::{Element, Feature, Geometry, GeometryType, MemberRecord, Meta, TagSelection};
use crate::ExportError;

/// Which geometries to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryTypes {
    /// Points from nodes.
    pub points: bool,
    /// Linestrings from ways (and multi-linestrings from relations, when
    /// [`FeatureOptions::relation_lines`] is set).
    pub lines: bool,
    /// Multipolygons from closed ways and area relations.
    pub polygons: bool,
}

impl Default for GeometryTypes {
    fn default() -> Self {
        Self {
            points: true,
            lines: true,
            polygons: true,
        }
    }
}

/// Where node locations are kept while ways and relations are built.
#[derive(Clone, Debug, Default)]
pub enum NodeStorage {
    /// Compressed, in memory (about 10 bytes per stored node).
    #[default]
    Memory,
    /// A dense memory-mapped sparse file at this path, which must not exist
    /// yet (it never outlives the export):
    /// best when hundreds of millions of nodes are needed.
    DenseFile(std::path::PathBuf),
}

/// Options shared by both exports.
#[derive(Clone, Debug)]
pub struct CommonOptions {
    /// Which elements are selected.
    pub filters: FilterSet,
    /// Which tags to emit.
    pub tags: TagSelection,
    /// Emit metadata (version, timestamp, changeset, user).
    pub metadata: bool,
    /// Only emit what intersects `[west, south, east, north]`. Element
    /// exports apply it to nodes, and to ways whose coordinates are known
    /// (`way_locations`, or files with `LocationsOnWays`); relations are not
    /// filtered.
    pub bbox: Option<[f64; 4]>,
    /// Worker threads (0: all cores).
    pub threads: usize,
    /// Where node locations are kept.
    pub node_storage: NodeStorage,
}

impl Default for CommonOptions {
    fn default() -> Self {
        Self {
            filters: FilterSet::default(),
            tags: TagSelection::All,
            metadata: false,
            bbox: None,
            threads: 0,
            node_storage: NodeStorage::Memory,
        }
    }
}

/// Options for [`export_features`].
#[derive(Clone, Debug, Default)]
pub struct FeatureOptions {
    /// Shared options.
    pub common: CommonOptions,
    /// Geometry types to produce.
    pub geometry_types: GeometryTypes,
    /// What closed ways become.
    pub closed_ways: ClosedWays,
    /// Also turn non-area relations (routes, waterways, …) into
    /// multi-linestrings.
    pub relation_lines: bool,
}

/// Options for [`export_elements`].
#[derive(Clone, Debug)]
pub struct ElementOptions {
    /// Shared options.
    pub common: CommonOptions,
    /// Emit nodes.
    pub nodes: bool,
    /// Emit ways.
    pub ways: bool,
    /// Emit relations.
    pub relations: bool,
    /// Give ways their node locations (needs a pass to collect them unless
    /// the file has `LocationsOnWays`).
    pub way_locations: bool,
}

impl Default for ElementOptions {
    fn default() -> Self {
        Self {
            common: CommonOptions::default(),
            nodes: true,
            ways: true,
            relations: true,
            way_locations: false,
        }
    }
}

/// A pass over the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    /// Finding the ways that selected relations need.
    ScanRelations,
    /// Finding the nodes that selected ways need.
    ScanWays,
    /// Reading nodes.
    Nodes,
    /// Reading ways.
    Ways,
    /// Reading relations.
    Relations,
    /// Reading everything in one pass (element export without locations).
    Elements,
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
            Phase::Elements => "elements",
        }
    }
}

/// Progress through a pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// The pass.
    pub phase: Phase,
    /// Blocks done in this pass.
    pub blocks_done: usize,
    /// Blocks in this pass.
    pub blocks_total: usize,
}

/// What the sink receives.
#[derive(Debug)]
pub enum Event<T> {
    /// Records from one block.
    Batch(Vec<T>),
    /// Progress (after each block).
    Progress(Progress),
}

/// Whether to keep going.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    /// Continue.
    Continue,
    /// Stop the export (it then returns normally with partial stats).
    Stop,
}

/// Counters from an export.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportStats {
    /// Records emitted.
    pub emitted: u64,
    /// Points emitted.
    pub points: u64,
    /// Linestrings emitted.
    pub lines: u64,
    /// Multi-linestrings emitted.
    pub multi_lines: u64,
    /// Multipolygons emitted.
    pub polygons: u64,
    /// Ways skipped because a node was missing from the file.
    pub ways_missing_nodes: u64,
    /// Relations skipped because member ways were missing from the file.
    pub relations_missing_ways: u64,
    /// Area relations whose ways did not close into rings or crossed.
    pub invalid_areas: u64,
    /// Node locations stored.
    pub stored_nodes: u64,
    /// Way geometries stored.
    pub stored_ways: u64,
    /// Approximate bytes held by the location stores at their peak (the
    /// in-memory stores; a dense node store file is not counted).
    pub store_bytes: u64,
    /// Whether the sink stopped the export early.
    pub stopped: bool,
    /// Wall-clock milliseconds.
    pub elapsed_ms: u64,
}

#[derive(Default)]
struct Counters {
    points: AtomicU64,
    lines: AtomicU64,
    multi_lines: AtomicU64,
    polygons: AtomicU64,
    ways_missing_nodes: AtomicU64,
    relations_missing_ways: AtomicU64,
    invalid_areas: AtomicU64,
}

impl Counters {
    fn add(&self, c: &AtomicU64, n: u64) {
        c.fetch_add(n, Ordering::Relaxed);
    }
}

fn threads(n: usize) -> usize {
    if n == 0 {
        tilemaker_pbf::parallel::default_threads()
    } else {
        n
    }
}

/// Whether a relation describes an area.
pub fn is_area_relation(tags: &Tags<'_>) -> bool {
    matches!(tags.get("type"), Some("multipolygon" | "boundary"))
}

struct Shared {
    filters: FilterSet,
    tags: TagSelection,
    metadata: bool,
    bbox: Option<[f64; 4]>,
    geometry_types: GeometryTypes,
    closed_ways: ClosedWays,
    relation_lines: bool,
    locations_on_ways: bool,
    /// Ways that selected relations need (filled, then frozen).
    needed_ways_builder: RwLock<Option<IdSetBuilder>>,
    needed_ways: RwLock<IdSet>,
    /// Nodes that selected or needed ways need (filled, then frozen).
    needed_nodes_builder: RwLock<Option<IdSetBuilder>>,
    needed_nodes: RwLock<IdSet>,
    nodes: RwLock<Option<NodeStoreBuilder>>,
    node_store: RwLock<Option<NodeStore>>,
    /// Relation-only selections: needed ways' node ids, kept during the
    /// ways scan so the ways need not be read a second time.
    way_refs: RwLock<Option<RefStoreBuilder>>,
    ref_store: RwLock<Option<RefStore>>,
    ways: RwLock<Option<WayStoreBuilder>>,
    way_store: RwLock<Option<WayStore>>,
    counters: Counters,
}

impl Shared {
    fn in_bbox(&self, b: [f64; 4]) -> bool {
        match self.bbox {
            None => true,
            Some(f) => !(b[2] < f[0] || b[0] > f[2] || b[3] < f[1] || b[1] > f[3]),
        }
    }

    fn way_is_closed(w: &Way<'_>) -> bool {
        let r = w.refs();
        if r.len() >= 4 {
            return r.first() == r.last();
        }
        let l = w.locations();
        l.len() >= 4 && l.first() == l.last()
    }

    /// (polygon, line) outputs for a selected way, if it is selected.
    fn way_outputs(&self, w: &Way<'_>, f: &BlockFilter<'_>) -> Option<(bool, bool)> {
        let closed = Self::way_is_closed(w);
        let tags = w.tags();
        let (poly, line) = if closed {
            self.closed_ways.outputs(&tags)
        } else {
            (false, true)
        };
        let poly = poly && self.geometry_types.polygons;
        let line = line && self.geometry_types.lines;
        if !poly && !line {
            return None;
        }
        f.matches(ElementType::Way, closed && poly, &tags)
            .then_some((poly, line))
    }

    /// Whether a relation is selected, and whether it is an area.
    fn relation_selected(&self, r: &Relation<'_>, f: &BlockFilter<'_>) -> Option<bool> {
        let tags = r.tags();
        let area = is_area_relation(&tags);
        let wanted = if area {
            self.geometry_types.polygons
        } else {
            self.relation_lines && self.geometry_types.lines
        };
        (wanted && f.matches(ElementType::Relation, area, &tags)).then_some(area)
    }
}

/// Stream assembled features. See the module docs for the passes.
pub fn export_features(
    file: &PbfFile,
    opts: &FeatureOptions,
    mut sink: impl FnMut(Event<Feature>) -> Flow,
) -> Result<ExportStats, ExportError> {
    let started = Instant::now();
    let c = &opts.common;
    let nthreads = threads(c.threads);
    let layout = file.layout()?;
    let gt = opts.geometry_types;
    let shared = Arc::new(Shared {
        filters: c.filters.clone(),
        tags: c.tags.clone(),
        metadata: c.metadata,
        bbox: c.bbox,
        geometry_types: gt,
        closed_ways: opts.closed_ways,
        relation_lines: opts.relation_lines,
        locations_on_ways: file.header().has_locations_on_ways(),
        needed_ways_builder: RwLock::new(Some(IdSetBuilder::new())),
        needed_ways: RwLock::new(IdSet::new()),
        needed_nodes_builder: RwLock::new(Some(IdSetBuilder::new())),
        needed_nodes: RwLock::new(IdSet::new()),
        nodes: RwLock::new(Some(new_node_builder(&c.node_storage)?)),
        node_store: RwLock::new(None),
        way_refs: RwLock::new(Some(RefStoreBuilder::new())),
        ref_store: RwLock::new(None),
        ways: RwLock::new(Some(WayStoreBuilder::new())),
        way_store: RwLock::new(None),
        counters: Counters::default(),
    });
    let want_points = gt.points && c.filters.may_select(ElementType::Node);
    let want_ways = (gt.lines || gt.polygons) && c.filters.may_select(ElementType::Way);
    let want_rels = (gt.polygons || (gt.lines && opts.relation_lines)) && c.filters.may_select(ElementType::Relation);

    let mut stats = ExportStats::default();
    let mut runner = Runner {
        file,
        threads: nthreads,
        layout: layout.clone(),
        stopped: false,
        emitted: 0,
    };
    let meta_opts = |mut o: DecodeOptions| {
        o.metadata = c.metadata;
        o
    };

    // 1. Relations scan.
    if want_rels {
        let s = shared.clone();
        runner.pass(
            Phase::ScanRelations,
            only(ElementType::Relation),
            &mut sink,
            move |_, b| {
                let guard = s.needed_ways_builder.read().unwrap();
                let set = guard.as_ref().unwrap();
                let bf = s.filters.for_block(b);
                for r in b.relations() {
                    if s.relation_selected(&r, &bf).is_none() {
                        continue;
                    }
                    // Every way member counts, whatever its role (as in
                    // osmium): roles are often missing or wrong.
                    for m in r.members() {
                        if m.kind == ElementType::Way {
                            set.insert(m.id);
                        }
                    }
                }
                Ok(Vec::new())
            },
        )?;
    }
    freeze_set(&shared.needed_ways_builder, &shared.needed_ways);
    let any_needed_ways = !shared.needed_ways.read().unwrap().is_empty();

    // 2. Ways scan. When no way is selected for itself (only as a member
    //    of selected relations), the needed ways' node ids are kept here and
    //    the ways are not read again (pass 4 is skipped).
    let refs_mode = !want_ways && !shared.locations_on_ways;
    if !runner.stopped && !shared.locations_on_ways && (want_ways || any_needed_ways) {
        let s = shared.clone();
        runner.pass(Phase::ScanWays, only(ElementType::Way), &mut sink, move |_, b| {
            let needed_ways = s.needed_ways.read().unwrap();
            let guard = s.needed_nodes_builder.read().unwrap();
            let set = guard.as_ref().unwrap();
            let mut refs = RefBatch::new();
            let bf = s.filters.for_block(b);
            for w in b.ways() {
                let selected = want_ways && s.way_outputs(&w, &bf).is_some();
                let needed = any_needed_ways && needed_ways.contains(w.id());
                if selected || needed {
                    set.insert_all(w.refs());
                }
                if refs_mode && needed {
                    refs.push(w.id(), w.refs());
                }
            }
            if !refs.is_empty() {
                s.way_refs.read().unwrap().as_ref().unwrap().insert(refs);
            }
            Ok(Vec::new())
        })?;
    }
    if let Some(b) = shared.way_refs.write().unwrap().take() {
        if refs_mode {
            let store = b.freeze();
            stats.stored_ways = store.len() as u64;
            stats.store_bytes += store.heap_bytes() as u64;
            *shared.ref_store.write().unwrap() = Some(store);
        }
    }
    freeze_set(&shared.needed_nodes_builder, &shared.needed_nodes);
    let any_needed_nodes = !shared.needed_nodes.read().unwrap().is_empty();

    // 3. Nodes.
    if !runner.stopped && (want_points || any_needed_nodes) {
        let s = shared.clone();
        runner.pass(
            Phase::Nodes,
            meta_opts(only(ElementType::Node)),
            &mut sink,
            move |_, b| {
                let needed_nodes = s.needed_nodes.read().unwrap();
                let mut out = Vec::new();
                let mut batch: Vec<(i64, Location)> = Vec::new();
                let bf = s.filters.for_block(b);
                for n in b.nodes() {
                    if any_needed_nodes && needed_nodes.contains(n.id()) {
                        batch.push((n.id(), (n.lat_e7(), n.lon_e7())));
                    }
                    if want_points && !n.tags().is_empty() && bf.matches(ElementType::Node, false, &n.tags()) {
                        let (lon, lat) = (n.lon(), n.lat());
                        if !s.in_bbox([lon, lat, lon, lat]) {
                            continue;
                        }
                        out.push(Feature {
                            kind: ElementType::Node,
                            id: n.id(),
                            tags: s.tags.apply(&n.tags()),
                            geometry: Geometry {
                                kind: GeometryType::Point,
                                coords: vec![lon, lat],
                                parts: Vec::new(),
                                polygons: Vec::new(),
                            },
                            meta: if s.metadata { Meta::from_ref(n.info()) } else { None },
                        });
                    }
                }
                if !batch.is_empty() {
                    s.nodes.read().unwrap().as_ref().unwrap().insert(&mut batch)?;
                }
                s.counters.add(&s.counters.points, out.len() as u64);
                Ok(out)
            },
        )?;
    }
    *shared.needed_nodes.write().unwrap() = IdSet::new();
    freeze_nodes(&shared, &mut stats);

    // 4. Ways (not needed when the scan kept the member ways' node ids).
    if !runner.stopped && (want_ways || (any_needed_ways && !refs_mode)) {
        let blocks = runner.blocks(Phase::Ways);
        let s = shared.clone();
        runner.pass_blocks(
            Phase::Ways,
            blocks,
            meta_opts(only(ElementType::Way)),
            &mut sink,
            move |_, b| {
                let needed_ways = s.needed_ways.read().unwrap();
                let store = s.node_store.read().unwrap();
                let mut out = Vec::new();
                let mut batch = WayBatch::new();
                let mut locs: Vec<Location> = Vec::new();
                let bf = s.filters.for_block(b);
                for w in b.ways() {
                    let outputs = if want_ways { s.way_outputs(&w, &bf) } else { None };
                    let needed = any_needed_ways && needed_ways.contains(w.id());
                    if outputs.is_none() && !needed {
                        continue;
                    }
                    if !resolve_way(&w, store.as_ref(), &mut locs) {
                        s.counters.add(&s.counters.ways_missing_nodes, 1);
                        continue;
                    }
                    if needed {
                        batch.push(w.id(), locs.iter().copied());
                    }
                    let Some((poly, line)) = outputs else { continue };
                    if locs.len() < 2 {
                        continue;
                    }
                    let coords: Vec<f64> = locs
                        .iter()
                        .flat_map(|&(lat, lon)| [lon as f64 / 1e7, lat as f64 / 1e7])
                        .collect();
                    let tags = s.tags.apply(&w.tags());
                    let meta = if s.metadata { Meta::from_ref(w.info()) } else { None };
                    let mut emit = |geometry: Geometry, tags: Vec<(String, String)>| {
                        if s.in_bbox(geometry.bbox()) {
                            let c = match geometry.kind {
                                GeometryType::MultiPolygon => &s.counters.polygons,
                                _ => &s.counters.lines,
                            };
                            s.counters.add(c, 1);
                            out.push(Feature {
                                kind: ElementType::Way,
                                id: w.id(),
                                tags,
                                geometry,
                                meta: meta.clone(),
                            });
                        }
                    };
                    if poly {
                        let mut g = Geometry {
                            kind: GeometryType::MultiPolygon,
                            coords: coords.clone(),
                            parts: vec![locs.len() as u32],
                            polygons: vec![1],
                        };
                        orient_ring_ccw(&mut g.coords);
                        emit(g, tags.clone());
                    }
                    if line {
                        emit(
                            Geometry {
                                kind: GeometryType::LineString,
                                coords,
                                parts: Vec::new(),
                                polygons: Vec::new(),
                            },
                            tags,
                        );
                    }
                }
                if !batch.is_empty() {
                    s.ways.read().unwrap().as_ref().unwrap().insert(batch);
                }
                Ok(out)
            },
        )?;
    }
    if !refs_mode {
        // (With way refs, relations resolve them against the node store.)
        drop(shared.node_store.write().unwrap().take());
    }
    *shared.needed_ways.write().unwrap() = IdSet::new();
    freeze_ways(&shared, &mut stats);

    // 5. Relations.
    if !runner.stopped && want_rels {
        let s = shared.clone();
        runner.pass_relations(meta_opts(only(ElementType::Relation)), &mut sink, move |b, range| {
            let way_store = s.way_store.read().unwrap();
            let way_store = way_store.as_ref().unwrap();
            let ref_store = s.ref_store.read().unwrap();
            let node_store = s.node_store.read().unwrap();
            let mut out = Vec::new();
            let mut buf = Vec::new();
            let mut ids = Vec::new();
            // A member way's locations, from the way store or from its node
            // ids and the node store; `false` when unknown.
            let mut way = |id: i64, buf: &mut Vec<Location>| -> bool {
                match (ref_store.as_ref(), node_store.as_ref()) {
                    (Some(rs), Some(ns)) => {
                        buf.clear();
                        rs.get_into(id, &mut ids) && ids.iter().all(|&n| ns.get(n).map(|l| buf.push(l)).is_some())
                    }
                    _ => way_store.get_into(id, buf),
                }
            };
            let bf = s.filters.for_block(b);
            for r in range.map(|i| b.relation(i)) {
                let Some(area) = s.relation_selected(&r, &bf) else {
                    continue;
                };
                let mut ways: Vec<Vec<LatpLon>> = Vec::new();
                let mut missing = false;
                // A way listed twice (under two roles) counts once, as in
                // osmium; twice would cancel its own segments.
                let mut seen = std::collections::HashSet::new();
                for m in r.members() {
                    if m.kind != ElementType::Way || !seen.insert(m.id) {
                        continue;
                    }
                    if way(m.id, &mut buf) {
                        // Topology only: lat goes in the latp slot unprojected.
                        ways.push(buf.iter().map(|&(lat, lon)| LatpLon { latp: lat, lon }).collect());
                    } else {
                        missing = true;
                    }
                }
                if missing {
                    // Like osmium: an incomplete relation (e.g. cut by an
                    // extract's edge) yields nothing rather than a partial shape.
                    s.counters.add(&s.counters.relations_missing_ways, 1);
                    continue;
                }
                if ways.is_empty() {
                    // No way members at all: nothing to draw.
                    continue;
                }
                let refs: Vec<&[LatpLon]> = ways.iter().map(|w| w.as_slice()).collect();
                let geometry = if area {
                    let a = assemble_area(&refs);
                    if a.open_rings > 0 || a.crossing || a.multipolygon.0.is_empty() {
                        s.counters.add(&s.counters.invalid_areas, 1);
                        continue;
                    }
                    multipolygon_geometry(&a.multipolygon)
                } else {
                    let runs = merge_ways(&refs);
                    if runs.is_empty() {
                        continue;
                    }
                    let mut g = Geometry {
                        kind: GeometryType::MultiLineString,
                        coords: Vec::new(),
                        parts: Vec::new(),
                        polygons: Vec::new(),
                    };
                    for run in runs {
                        g.coords
                            .extend(run.iter().flat_map(|ll| [ll.lon as f64 / 1e7, ll.latp as f64 / 1e7]));
                        g.parts.push((g.coords.len() / 2) as u32);
                    }
                    g
                };
                if !s.in_bbox(geometry.bbox()) {
                    continue;
                }
                let c = match geometry.kind {
                    GeometryType::MultiPolygon => &s.counters.polygons,
                    _ => &s.counters.multi_lines,
                };
                s.counters.add(c, 1);
                out.push(Feature {
                    kind: ElementType::Relation,
                    id: r.id(),
                    tags: s.tags.apply(&r.tags()),
                    geometry,
                    meta: if s.metadata { Meta::from_ref(r.info()) } else { None },
                });
            }
            Ok(out)
        })?;
    }
    drop(shared.way_store.write().unwrap().take());
    drop(shared.ref_store.write().unwrap().take());
    drop(shared.node_store.write().unwrap().take());

    let k = &shared.counters;
    stats.points = k.points.load(Ordering::Relaxed);
    stats.lines = k.lines.load(Ordering::Relaxed);
    stats.multi_lines = k.multi_lines.load(Ordering::Relaxed);
    stats.polygons = k.polygons.load(Ordering::Relaxed);
    stats.ways_missing_nodes = k.ways_missing_nodes.load(Ordering::Relaxed);
    stats.relations_missing_ways = k.relations_missing_ways.load(Ordering::Relaxed);
    stats.invalid_areas = k.invalid_areas.load(Ordering::Relaxed);
    stats.emitted = runner.emitted;
    stats.stopped = runner.stopped;
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(stats)
}

fn freeze_set(builder: &RwLock<Option<IdSetBuilder>>, into: &RwLock<IdSet>) {
    if let Some(b) = builder.write().unwrap().take() {
        *into.write().unwrap() = b.freeze();
    }
}

fn new_node_builder(s: &NodeStorage) -> Result<NodeStoreBuilder, ExportError> {
    Ok(match s {
        NodeStorage::Memory => NodeStore::memory(),
        NodeStorage::DenseFile(p) => NodeStore::dense_file(p)?,
    })
}

fn freeze_nodes(shared: &Shared, stats: &mut ExportStats) {
    if let Some(b) = shared.nodes.write().unwrap().take() {
        let store = b.freeze();
        stats.stored_nodes = store.len() as u64;
        stats.store_bytes += store.heap_bytes() as u64;
        *shared.node_store.write().unwrap() = Some(store);
    }
}

fn freeze_ways(shared: &Shared, stats: &mut ExportStats) {
    if let Some(b) = shared.ways.write().unwrap().take() {
        let store = b.freeze();
        stats.stored_ways = stats.stored_ways.max(store.len() as u64);
        // The node store is alive while ways are stored: their sum is the peak.
        stats.store_bytes += store.heap_bytes() as u64;
        *shared.way_store.write().unwrap() = Some(store);
    }
}

/// A way's locations from the node store (or the way itself); `false` when
/// a node is missing.
fn resolve_way(w: &Way<'_>, store: Option<&NodeStore>, out: &mut Vec<Location>) -> bool {
    out.clear();
    let l = w.locations();
    if !l.is_empty() {
        out.extend_from_slice(l);
        return true;
    }
    let Some(store) = store else { return w.refs().is_empty() };
    for &id in w.refs() {
        match store.get(id) {
            Some(loc) => out.push(loc),
            None => return false,
        }
    }
    true
}

fn orient_ring_ccw(coords: &mut [f64]) {
    let n = coords.len() / 2;
    let mut sum = 0.0;
    for i in 0..n.saturating_sub(1) {
        sum += coords[2 * i] * coords[2 * i + 3] - coords[2 * i + 2] * coords[2 * i + 1];
    }
    if sum < 0.0 {
        let pairs: Vec<[f64; 2]> = coords.chunks_exact(2).rev().map(|c| [c[0], c[1]]).collect();
        for (i, p) in pairs.into_iter().enumerate() {
            coords[2 * i] = p[0];
            coords[2 * i + 1] = p[1];
        }
    }
}

fn multipolygon_geometry(mp: &tilemaker_geom::MultiPolygon) -> Geometry {
    let mut g = Geometry {
        kind: GeometryType::MultiPolygon,
        coords: Vec::new(),
        parts: Vec::new(),
        polygons: Vec::new(),
    };
    for p in &mp.0 {
        for ring in std::iter::once(p.exterior()).chain(p.interiors()) {
            g.coords.extend(ring.0.iter().flat_map(|c| [c.x, c.y]));
            g.parts.push((g.coords.len() / 2) as u32);
        }
        g.polygons.push(g.parts.len() as u32);
    }
    g
}

fn only(kind: ElementType) -> DecodeOptions {
    DecodeOptions {
        nodes: kind == ElementType::Node,
        ways: kind == ElementType::Way,
        relations: kind == ElementType::Relation,
        metadata: false,
    }
}

struct Runner<'a> {
    file: &'a PbfFile,
    threads: usize,
    layout: Layout,
    stopped: bool,
    emitted: u64,
}

/// Work units of the relations pass hold relations until they have this
/// many members in total: assembling a country can take seconds, and its
/// chunk should not also hold thousands of small relations whose output
/// would wait for it.
const RELATION_CHUNK_MEMBERS: usize = 512;

/// Split a block's relations into chunks of about
/// [`RELATION_CHUNK_MEMBERS`] members (always at least one relation).
fn relation_chunks(block: &PrimitiveBlock) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut members = 0;
    for i in 0..block.relation_count() {
        members += block.relation(i).member_count().max(1);
        if members >= RELATION_CHUNK_MEMBERS {
            out.push(start..i + 1);
            start = i + 1;
            members = 0;
        }
    }
    if start < block.relation_count() || out.is_empty() {
        out.push(start..block.relation_count());
    }
    out
}

impl Runner<'_> {
    /// The relations pass, parallel over small chunks of relations.
    fn pass_relations<T: Send + 'static>(
        &mut self,
        opts: DecodeOptions,
        sink: &mut impl FnMut(Event<T>) -> Flow,
        f: impl Fn(&PrimitiveBlock, std::ops::Range<usize>) -> Result<Vec<T>, ExportError> + Send + Sync + 'static,
    ) -> Result<(), ExportError> {
        if self.stopped {
            return Ok(());
        }
        let range = self.layout.relations.clone();
        let total = range.len();
        let blocks = self.file.blocks(range, opts, self.threads.min(4));
        let chunks = blocks
            .enumerate()
            .flat_map(|(n, b)| -> Box<dyn Iterator<Item = _> + Send> {
                match b {
                    Ok((_, block)) => {
                        let chunks = relation_chunks(&block);
                        let last = chunks.len() - 1;
                        let block = Arc::new(block);
                        Box::new(
                            chunks
                                .into_iter()
                                .enumerate()
                                .map(move |(c, r)| Ok((n, block.clone(), r, c == last))),
                        )
                    }
                    Err(e) => Box::new(std::iter::once(Err(e))),
                }
            });
        let results = tilemaker_pbf::parallel::ordered_map(chunks, self.threads, self.threads * 8, move |item| {
            let (n, block, r, last) = item?;
            Ok::<_, ExportError>((n, last, f(&block, r)?))
        });
        for r in results {
            let (n, last, batch) = r?;
            if !batch.is_empty() {
                self.emitted += batch.len() as u64;
                if sink(Event::Batch(batch)) == Flow::Stop {
                    self.stopped = true;
                    return Ok(());
                }
            }
            if last {
                let p = Progress {
                    phase: Phase::Relations,
                    blocks_done: n + 1,
                    blocks_total: total,
                };
                if sink(Event::Progress(p)) == Flow::Stop {
                    self.stopped = true;
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    /// The blocks a pass reads: those that can hold its element type.
    fn blocks(&self, phase: Phase) -> Vec<usize> {
        let range = match phase {
            Phase::ScanRelations | Phase::Relations => self.layout.relations.clone(),
            Phase::ScanWays | Phase::Ways => self.layout.ways.clone(),
            Phase::Nodes => self.layout.nodes.clone(),
            Phase::Elements => self.layout.nodes.start..self.layout.relations.end,
        };
        range.collect()
    }

    fn pass<T: Send + 'static>(
        &mut self,
        phase: Phase,
        opts: DecodeOptions,
        sink: &mut impl FnMut(Event<T>) -> Flow,
        f: impl Fn(usize, &PrimitiveBlock) -> Result<Vec<T>, ExportError> + Send + Sync + 'static,
    ) -> Result<(), ExportError> {
        let blocks = self.blocks(phase);
        self.pass_blocks(phase, blocks, opts, sink, f)
    }

    /// A pass over a given list of blocks.
    fn pass_blocks<T: Send + 'static>(
        &mut self,
        phase: Phase,
        blocks: Vec<usize>,
        opts: DecodeOptions,
        sink: &mut impl FnMut(Event<T>) -> Flow,
        f: impl Fn(usize, &PrimitiveBlock) -> Result<Vec<T>, ExportError> + Send + Sync + 'static,
    ) -> Result<(), ExportError> {
        if self.stopped {
            return Ok(());
        }
        let total = blocks.len();
        let results = self.file.map_block_list(blocks, opts, self.threads, move |i, block| {
            f(i, &block).map_err(|e| match e {
                ExportError::Pbf(p) => p,
                other => tilemaker_pbf::Error::Unsupported(other.to_string()),
            })
        });
        for (done, r) in results.enumerate() {
            let batch = r?;
            if !batch.is_empty() {
                self.emitted += batch.len() as u64;
                if sink(Event::Batch(batch)) == Flow::Stop {
                    self.stopped = true;
                    return Ok(());
                }
            }
            let p = Progress {
                phase,
                blocks_done: done + 1,
                blocks_total: total,
            };
            if sink(Event::Progress(p)) == Flow::Stop {
                self.stopped = true;
                return Ok(());
            }
        }
        Ok(())
    }
}

/// Stream elements from any byte stream (a download, stdin, a socket) in
/// one pass, decoding blocks on worker threads as they arrive, in order.
///
/// Way locations need a second look at the data, so they are only
/// available here when the file has `LocationsOnWays`; otherwise use
/// [`export_elements`] on a file. Progress reports blocks done with a
/// total of 0 (unknown).
pub fn export_elements_from_reader<R: std::io::Read + Send + 'static>(
    reader: R,
    opts: &ElementOptions,
    mut sink: impl FnMut(Event<Element>) -> Flow,
) -> Result<ExportStats, ExportError> {
    use tilemaker_pbf::{decompress_blob, BlobKind, BlobReader, Header};

    let started = Instant::now();
    let c = &opts.common;
    let nthreads = threads(c.threads);
    let mut blobs = BlobReader::new(reader);
    // The header comes first.
    let mut header = None;
    let mut first_data = None;
    for blob in blobs.by_ref() {
        let (info, raw) = blob?;
        match info.kind {
            BlobKind::Header => {
                let mut out = Vec::new();
                decompress_blob(&raw, &mut out)?;
                header = Some(Header::decode(&out)?);
            }
            BlobKind::Data => {
                first_data = Some(raw);
                break;
            }
            BlobKind::Unknown => {}
        }
    }
    let header = header.ok_or(tilemaker_pbf::Error::Corrupt("no OSMHeader blob"))?;
    header.check_supported()?;
    if opts.ways && opts.way_locations && !header.has_locations_on_ways() {
        return Err(ExportError::Invalid(
            "way locations from a stream need a file with LocationsOnWays; read from a file instead".into(),
        ));
    }
    let shared = Arc::new(ElemShared {
        filters: c.filters.clone(),
        tags: c.tags.clone(),
        metadata: c.metadata,
        bbox: c.bbox,
        nodes: opts.nodes,
        ways: opts.ways,
        relations: opts.relations,
        way_locations: opts.way_locations,
        needed_nodes_builder: RwLock::new(None),
        needed_nodes: RwLock::new(IdSet::new()),
        builder: RwLock::new(None),
        store: RwLock::new(None),
    });
    let decode = DecodeOptions {
        nodes: opts.nodes && c.filters.may_select(ElementType::Node),
        ways: opts.ways && c.filters.may_select(ElementType::Way),
        relations: opts.relations && c.filters.may_select(ElementType::Relation),
        metadata: c.metadata,
    };
    let data = first_data.map(Ok).into_iter().chain(blobs.filter_map(|b| match b {
        Ok((info, raw)) => (info.kind == BlobKind::Data).then_some(Ok(raw)),
        Err(e) => Some(Err(e)),
    }));
    let s = shared.clone();
    let results = tilemaker_pbf::parallel::ordered_map(data, nthreads, nthreads * 2 + 2, move |raw| {
        let raw = raw?;
        let mut out = Vec::new();
        decompress_blob(&raw, &mut out)?;
        let b = PrimitiveBlock::decode(&out, decode)?;
        let bf = s.filters.for_block(&b);
        let mut els: Vec<Element> = Vec::new();
        els.extend(b.nodes().filter_map(|x| s.node(&x, &bf)));
        els.extend(b.ways().filter_map(|x| s.way(&x, &bf, None)));
        els.extend(b.relations().filter_map(|x| s.relation(&x, &bf)));
        Ok::<_, tilemaker_pbf::Error>(els)
    });
    let mut stats = ExportStats::default();
    for (done, r) in results.enumerate() {
        let batch = r?;
        if !batch.is_empty() {
            stats.emitted += batch.len() as u64;
            if sink(Event::Batch(batch)) == Flow::Stop {
                stats.stopped = true;
                break;
            }
        }
        let p = Progress {
            phase: Phase::Elements,
            blocks_done: done + 1,
            blocks_total: 0,
        };
        if sink(Event::Progress(p)) == Flow::Stop {
            stats.stopped = true;
            break;
        }
    }
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(stats)
}

/// Stream raw elements (nodes, ways, relations) that match the filters.
/// Without way locations this is a single pass using constant memory.
pub fn export_elements(
    file: &PbfFile,
    opts: &ElementOptions,
    mut sink: impl FnMut(Event<Element>) -> Flow,
) -> Result<ExportStats, ExportError> {
    let started = Instant::now();
    let c = &opts.common;
    let nthreads = threads(c.threads);
    let layout = file.layout()?;
    let low = file.header().has_locations_on_ways();
    let need_store = opts.ways && opts.way_locations && !low;
    let shared = Arc::new(ElemShared {
        filters: c.filters.clone(),
        tags: c.tags.clone(),
        metadata: c.metadata,
        bbox: c.bbox,
        nodes: opts.nodes,
        ways: opts.ways,
        relations: opts.relations,
        way_locations: opts.way_locations,
        needed_nodes_builder: RwLock::new(Some(IdSetBuilder::new())),
        needed_nodes: RwLock::new(IdSet::new()),
        builder: RwLock::new(if need_store {
            Some(new_node_builder(&c.node_storage)?)
        } else {
            None
        }),
        store: RwLock::new(None),
    });
    let mut runner = Runner {
        file,
        threads: nthreads,
        layout: layout.clone(),
        stopped: false,
        emitted: 0,
    };
    let mut stats = ExportStats::default();
    let decode = |nodes: bool, ways: bool, relations: bool| DecodeOptions {
        nodes,
        ways,
        relations,
        metadata: c.metadata,
    };

    if need_store {
        let s = shared.clone();
        runner.pass(Phase::ScanWays, only(ElementType::Way), &mut sink, move |_, b| {
            let guard = s.needed_nodes_builder.read().unwrap();
            let set = guard.as_ref().unwrap();
            let bf = s.filters.for_block(b);
            for w in b.ways() {
                if s.way_selected(&w, &bf) {
                    set.insert_all(w.refs());
                }
            }
            Ok(Vec::new())
        })?;
        freeze_set(&shared.needed_nodes_builder, &shared.needed_nodes);
    }

    if need_store {
        // Nodes (storing locations), then ways and relations.
        let s = shared.clone();
        runner.pass(Phase::Nodes, decode(true, false, false), &mut sink, move |_, b| {
            let needed = s.needed_nodes.read().unwrap();
            let mut batch = Vec::new();
            let mut out = Vec::new();
            let bf = s.filters.for_block(b);
            for n in b.nodes() {
                if needed.contains(n.id()) {
                    batch.push((n.id(), (n.lat_e7(), n.lon_e7())));
                }
                if let Some(e) = s.node(&n, &bf) {
                    out.push(e);
                }
            }
            if !batch.is_empty() {
                s.builder.read().unwrap().as_ref().unwrap().insert(&mut batch)?;
            }
            Ok(out)
        })?;
        *shared.needed_nodes.write().unwrap() = IdSet::new();
        if let Some(b) = shared.builder.write().unwrap().take() {
            let store = b.freeze();
            stats.stored_nodes = store.len() as u64;
            stats.store_bytes = store.heap_bytes() as u64;
            *shared.store.write().unwrap() = Some(store);
        }
        let s = shared.clone();
        runner.pass(Phase::Ways, decode(false, true, false), &mut sink, move |_, b| {
            let store = s.store.read().unwrap();
            let bf = s.filters.for_block(b);
            Ok(b.ways().filter_map(|w| s.way(&w, &bf, store.as_ref())).collect())
        })?;
        drop(shared.store.write().unwrap().take());
        if opts.relations && c.filters.may_select(ElementType::Relation) {
            let s = shared.clone();
            runner.pass(Phase::Relations, decode(false, false, true), &mut sink, move |_, b| {
                let bf = s.filters.for_block(b);
                Ok(b.relations().filter_map(|r| s.relation(&r, &bf)).collect())
            })?;
        }
    } else {
        // One pass in file order, over the blocks that can hold the types
        // wanted (all of them for unsorted files).
        let s = shared.clone();
        let (dn, dw, dr) = (
            opts.nodes && c.filters.may_select(ElementType::Node),
            opts.ways && c.filters.may_select(ElementType::Way),
            opts.relations && c.filters.may_select(ElementType::Relation),
        );
        let n = file.blobs().len();
        let start = if dn {
            0
        } else if dw {
            layout.ways.start
        } else if dr {
            layout.relations.start
        } else {
            n
        };
        let end = if dr {
            n
        } else if dw {
            layout.ways.end
        } else if dn {
            layout.nodes.end
        } else {
            n
        };
        let span = Layout {
            nodes: start..end.max(start),
            ways: 0..0,
            relations: end.max(start)..end.max(start),
        };
        runner.layout = span;
        runner.pass(Phase::Elements, decode(dn, dw, dr), &mut sink, move |_, b| {
            let mut out: Vec<Element> = Vec::new();
            let bf = s.filters.for_block(b);
            out.extend(b.nodes().filter_map(|x| s.node(&x, &bf)));
            out.extend(b.ways().filter_map(|x| s.way(&x, &bf, None)));
            out.extend(b.relations().filter_map(|x| s.relation(&x, &bf)));
            Ok(out)
        })?;
    }
    stats.emitted = runner.emitted;
    stats.stopped = runner.stopped;
    stats.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(stats)
}

struct ElemShared {
    filters: FilterSet,
    tags: TagSelection,
    metadata: bool,
    bbox: Option<[f64; 4]>,
    nodes: bool,
    ways: bool,
    relations: bool,
    way_locations: bool,
    needed_nodes_builder: RwLock<Option<IdSetBuilder>>,
    needed_nodes: RwLock<IdSet>,
    builder: RwLock<Option<NodeStoreBuilder>>,
    store: RwLock<Option<NodeStore>>,
}

impl ElemShared {
    fn in_bbox(&self, lon: f64, lat: f64) -> bool {
        match self.bbox {
            None => true,
            Some(f) => lon >= f[0] && lon <= f[2] && lat >= f[1] && lat <= f[3],
        }
    }

    /// Whether `[lon, lat, …]` (NaN for unknown nodes) intersects the bbox:
    /// its bounding box overlaps it, as for features.
    fn coords_in_bbox(&self, v: &[f64]) -> bool {
        let Some(f) = self.bbox else { return true };
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for c in v.chunks_exact(2).filter(|c| c[0].is_finite()) {
            b = [b[0].min(c[0]), b[1].min(c[1]), b[2].max(c[0]), b[3].max(c[1])];
        }
        b[0].is_finite() && !(b[2] < f[0] || b[0] > f[2] || b[3] < f[1] || b[1] > f[3])
    }

    fn way_selected(&self, w: &Way<'_>, f: &BlockFilter<'_>) -> bool {
        if !self.ways {
            return false;
        }
        let closed = Shared::way_is_closed(w);
        let area = closed && crate::area::is_area_by_tags(&w.tags());
        f.matches(ElementType::Way, area, &w.tags())
    }

    fn meta(&self, i: Option<tilemaker_pbf::InfoRef<'_>>) -> Option<Meta> {
        if self.metadata {
            Meta::from_ref(i)
        } else {
            None
        }
    }

    fn node(&self, n: &tilemaker_pbf::Node<'_>, f: &BlockFilter<'_>) -> Option<Element> {
        if !self.nodes || !f.matches(ElementType::Node, false, &n.tags()) {
            return None;
        }
        let (lon, lat) = (n.lon(), n.lat());
        if !self.in_bbox(lon, lat) {
            return None;
        }
        Some(Element {
            kind: ElementType::Node,
            id: n.id(),
            tags: self.tags.apply(&n.tags()),
            location: Some([lon, lat]),
            refs: Vec::new(),
            locations: None,
            members: Vec::new(),
            meta: self.meta(n.info()),
        })
    }

    fn way(&self, w: &Way<'_>, f: &BlockFilter<'_>, store: Option<&NodeStore>) -> Option<Element> {
        if !self.way_selected(w, f) {
            return None;
        }
        let locations = if self.way_locations {
            let l = w.locations();
            let v: Vec<f64> = if !l.is_empty() {
                l.iter()
                    .flat_map(|&(lat, lon)| [lon as f64 / 1e7, lat as f64 / 1e7])
                    .collect()
            } else {
                w.refs()
                    .iter()
                    .flat_map(|&id| match store.and_then(|s| s.get(id)) {
                        Some((lat, lon)) => [lon as f64 / 1e7, lat as f64 / 1e7],
                        None => [f64::NAN, f64::NAN],
                    })
                    .collect()
            };
            if !self.coords_in_bbox(&v) {
                return None;
            }
            Some(v)
        } else {
            // Without wayLocations a way can still be placed when the file
            // carries its locations.
            let l = w.locations();
            if self.bbox.is_some() && !l.is_empty() {
                let v: Vec<f64> = l
                    .iter()
                    .flat_map(|&(lat, lon)| [lon as f64 / 1e7, lat as f64 / 1e7])
                    .collect();
                if !self.coords_in_bbox(&v) {
                    return None;
                }
            }
            None
        };
        Some(Element {
            kind: ElementType::Way,
            id: w.id(),
            tags: self.tags.apply(&w.tags()),
            location: None,
            refs: w.refs().to_vec(),
            locations,
            members: Vec::new(),
            meta: self.meta(w.info()),
        })
    }

    fn relation(&self, r: &Relation<'_>, f: &BlockFilter<'_>) -> Option<Element> {
        let area = is_area_relation(&r.tags());
        if !self.relations || !f.matches(ElementType::Relation, area, &r.tags()) {
            return None;
        }
        Some(Element {
            kind: ElementType::Relation,
            id: r.id(),
            tags: self.tags.apply(&r.tags()),
            location: None,
            refs: Vec::new(),
            locations: None,
            members: r
                .members()
                .map(|m| MemberRecord {
                    kind: m.kind,
                    id: m.id,
                    role: m.role.to_string(),
                })
                .collect(),
            meta: self.meta(r.info()),
        })
    }
}
