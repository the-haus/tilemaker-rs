//! Reading the OSM file through the Lua profile (port of
//! `PbfProcessor`): scan relations, (scan ways), nodes, ways, relations.
//! Blocks are decoded and processed on a rayon pool with one Lua state per
//! thread; results are merged into [`TileData`] on the calling thread.

use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rayon::prelude::*;
use tilemaker_geom::coords::{lat2latp, LatpLon};
use tilemaker_lua::{LuaProfile, MemberKind, Object, ObjectKind, ScannedRelations, Shared};
use tilemaker_pbf::{DecodeOptions, ElementType, PbfFile, PrimitiveBlock, Tags};
use tilemaker_store::{IdSet, IdSetBuilder, Location, NodeStore, NodeStoreBuilder, WayBatch, WayStoreBuilder};

use crate::data::{coverage, Footprint, IndexedObject, TileData};
use crate::significant::SignificantTags;
use crate::{CoreError, Flow, Phase, Progress};

/// Counters from reading.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadStats {
    /// Nodes handed to `node_function`.
    pub nodes_processed: u64,
    /// Ways handed to the profile.
    pub ways_processed: u64,
    /// Relations handed to the profile.
    pub relations_processed: u64,
    /// Relations accepted in `relation_scan_function`.
    pub relations_accepted: u64,
    /// Ways skipped because a node was missing.
    pub ways_missing_nodes: u64,
    /// Output objects (Layer calls that produced geometry).
    pub objects: u64,
    /// Node locations stored.
    pub stored_nodes: u64,
    /// Way geometries stored for relations.
    pub stored_ways: u64,
}

fn latp_lon((lat, lon): Location) -> LatpLon {
    LatpLon {
        latp: (lat2latp(lat as f64 / 1e7) * 1e7) as i32,
        lon,
    }
}

fn owned_tags(t: &Tags<'_>) -> Vec<(String, String)> {
    t.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

struct Profiles {
    source: Arc<str>,
    name: String,
    shared: Arc<Shared>,
    slots: Vec<OnceLock<Mutex<LuaProfile>>>,
    first_error: Mutex<Option<String>>,
}

impl Profiles {
    /// This pool thread's profile.
    fn with<R>(&self, f: impl FnOnce(&mut LuaProfile) -> Result<R, CoreError>) -> Result<R, CoreError> {
        let i = rayon::current_thread_index().unwrap_or(0);
        let slot = &self.slots[i];
        if slot.get().is_none() {
            let p = LuaProfile::load(&self.source, &self.name, self.shared.clone(), i == 0)?;
            let _ = slot.set(Mutex::new(p));
        }
        let mut p = slot.get().unwrap().lock().unwrap();
        f(&mut p)
    }

    fn for_each(&self, mut f: impl FnMut(&LuaProfile)) {
        for s in &self.slots {
            if let Some(p) = s.get() {
                f(&p.lock().unwrap());
            }
        }
    }
}

pub(crate) struct Reader<'a> {
    pub file: &'a PbfFile,
    pub pool: &'a rayon::ThreadPool,
    pub threads: usize,
    pub progress: &'a mut dyn FnMut(Progress) -> Flow,
    pub stop: Arc<AtomicBool>,
}

impl Reader<'_> {
    /// Run `f` over `range` on the pool; `merge` each result here.
    fn phase<R: Send>(
        &mut self,
        phase: Phase,
        range: Range<usize>,
        opts: DecodeOptions,
        f: impl Fn(PrimitiveBlock) -> Result<R, CoreError> + Sync,
        mut merge: impl FnMut(R),
    ) -> Result<(), CoreError> {
        let total = range.len();
        if total == 0 || self.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let (tx, rx) = crossbeam_channel::bounded::<Result<R, CoreError>>(self.threads * 2);
        let file = self.file;
        let pool = self.pool;
        let stop = self.stop.clone();
        let f = &f;
        std::thread::scope(|s| -> Result<(), CoreError> {
            s.spawn(move || {
                pool.install(|| {
                    range.into_par_iter().for_each_with(tx, |tx, i| {
                        if stop.load(Ordering::Relaxed) {
                            return;
                        }
                        let r = file.decode_block(i, opts).map_err(CoreError::from).and_then(f);
                        if r.is_err() {
                            stop.store(true, Ordering::Relaxed);
                        }
                        let _ = tx.send(r);
                    })
                })
            });
            let mut done = 0;
            let mut first_err = None;
            for r in rx {
                match r {
                    Ok(r) => merge(r),
                    Err(e) => {
                        first_err.get_or_insert(e);
                        continue;
                    }
                }
                done += 1;
                if (self.progress)(Progress::Read {
                    phase,
                    blocks_done: done,
                    blocks_total: total,
                }) == Flow::Stop
                {
                    self.stop.store(true, Ordering::Relaxed);
                }
            }
            match first_err {
                Some(e) => Err(e),
                None => Ok(()),
            }
        })
    }
}

/// Read the file through the profile into tile data.
pub(crate) fn read(
    r: &mut Reader<'_>,
    config: &tilemaker_config::Config,
    profile_source: &str,
    profile_name: &str,
    shared: Arc<Shared>,
) -> Result<(TileData, ReadStats), CoreError> {
    let layout = r.file.layout()?;
    let profiles = Arc::new(Profiles {
        source: profile_source.into(),
        name: profile_name.to_string(),
        shared: shared.clone(),
        slots: (0..r.threads.max(1)).map(|_| OnceLock::new()).collect(),
        first_error: Mutex::new(None),
    });
    // Load one profile up front: syntax errors surface before any reading,
    // and its capabilities decide which passes run.
    let caps = r.pool.install(|| profiles.with(|p| Ok(p.capabilities().clone())))?;
    let node_keys = match &caps.node_keys {
        Some(k) => SignificantTags::new(k).map_err(CoreError::Profile)?,
        None => SignificantTags::all(),
    };
    let way_keys = match &caps.way_keys {
        Some(k) => SignificantTags::new(k).map_err(CoreError::Profile)?,
        None => SignificantTags::all(),
    };
    let base_zoom = config.basezoom;
    let mut data = TileData::new(base_zoom);
    let mut stats = ReadStats::default();
    let counters = Counters::default();
    let only = |kind: ElementType| DecodeOptions {
        nodes: kind == ElementType::Node,
        ways: kind == ElementType::Way,
        relations: kind == ElementType::Relation,
        metadata: false,
    };

    // 1. Relations: multipolygons mark their ways; others go through
    //    relation_scan_function.
    let used_ways_b = IdSetBuilder::new();
    let used_relations_b = IdSetBuilder::new();
    let node_filtering = way_keys.enabled();
    let used_nodes_b = IdSetBuilder::new();
    let mut scanned = ScannedRelations::new();
    {
        let wk = &way_keys;
        let (uw, ur, un) = (&used_ways_b, &used_relations_b, &used_nodes_b);
        let profiles = profiles.clone();
        let caps = &caps;
        r.phase(
            Phase::ScanRelations,
            layout.relations.clone(),
            only(ElementType::Relation),
            move |b| {
                let mut local = ScannedRelations::new();
                let mut accepted = 0u64;
                for rel in b.relations() {
                    let tags = rel.tags();
                    let mp = tags.get("type") == Some("multipolygon");
                    let mut is_accepted = false;
                    if !mp {
                        if caps.relation_scan {
                            is_accepted = profiles.with(|p| Ok(p.scan_relation(rel.id(), owned_tags(&tags))?))?;
                        }
                        if !is_accepted {
                            continue;
                        }
                        accepted += 1;
                        local.store_tags(rel.id(), owned_tags(&tags));
                    } else if !wk.filter(&tags) {
                        continue;
                    }
                    ur.insert(rel.id());
                    for m in rel.members() {
                        match m.kind {
                            ElementType::Way => {
                                uw.insert(m.id);
                                if is_accepted {
                                    local.add_member(rel.id(), MemberKind::Way, m.id, m.role);
                                }
                            }
                            ElementType::Node => {
                                if node_filtering {
                                    un.insert(m.id);
                                }
                                if is_accepted {
                                    local.add_member(rel.id(), MemberKind::Node, m.id, m.role);
                                }
                            }
                            ElementType::Relation => {
                                if is_accepted {
                                    local.add_member(rel.id(), MemberKind::Relation, m.id, m.role);
                                }
                            }
                        }
                    }
                }
                Ok((local, accepted))
            },
            |(local, accepted)| {
                scanned.merge(local);
                stats.relations_accepted += accepted;
            },
        )?;
    }
    if caps.relation_postscan && !scanned.is_empty() {
        let ids = scanned.relations_in_relations();
        let s = std::mem::take(&mut scanned);
        scanned = r.pool.install(|| profiles.with(|p| Ok(p.postscan(s, &ids)?)))?;
    }
    let scanned = Arc::new(scanned);
    {
        let sc = scanned.clone();
        profiles.for_each(|p| p.set_scanned_relations(sc.clone()));
    }
    let used_ways = used_ways_b.freeze();
    let used_relations = used_relations_b.freeze();

    // 2. With way_keys, only nodes of significant or used ways are kept.
    if node_filtering {
        let (wk, uw, un) = (&way_keys, &used_ways, &used_nodes_b);
        r.phase(
            Phase::ScanWays,
            layout.ways.clone(),
            only(ElementType::Way),
            move |b| {
                for w in b.ways() {
                    if uw.contains(w.id()) || wk.filter(&w.tags()) {
                        un.insert_all(w.refs());
                    }
                }
                Ok(())
            },
            |_| {},
        )?;
    }
    let used_nodes: Option<IdSet> = node_filtering.then(|| used_nodes_b.freeze());

    // 3. Nodes.
    let nodes_builder: NodeStoreBuilder = NodeStore::memory();
    {
        let (nk, un, nb, sc) = (&node_keys, &used_nodes, &nodes_builder, &scanned);
        let profiles = profiles.clone();
        let caps = &caps;
        let counters = &counters;
        r.phase(
            Phase::Nodes,
            layout.nodes.clone(),
            only(ElementType::Node),
            move |b| {
                let mut out = Vec::new();
                let mut batch: Vec<(i64, Location)> = Vec::new();
                for n in b.nodes() {
                    let loc = (n.lat_e7(), n.lon_e7());
                    let tags = n.tags();
                    let mut emitted = false;
                    if caps.nodes && !tags.is_empty() && nk.filter(&tags) {
                        let obj = Object {
                            kind: ObjectKind::Node,
                            id: n.id(),
                            tags: owned_tags(&tags),
                            location: Some(latp_lon(loc)),
                            relations: if sc.is_empty() {
                                Vec::new()
                            } else {
                                sc.relations_for(MemberKind::Node, n.id())
                            },
                            ..Object::default()
                        };
                        counters.nodes.fetch_add(1, Ordering::Relaxed);
                        let processed = profiles.with(|p| Ok(p.process(obj)?))?;
                        if !processed.is_empty() {
                            emitted = true;
                            out.push(coverage(processed, base_zoom, Footprint::Node));
                        }
                    }
                    let _ = emitted;
                    if un.as_ref().is_none_or(|u| u.contains(n.id())) {
                        batch.push((n.id(), loc));
                    }
                }
                nb.insert(&mut batch)?;
                Ok(out)
            },
            |objs: Vec<IndexedObject>| {
                for o in objs {
                    data.add(o);
                }
            },
        )?;
    }
    drop(used_nodes);
    let node_store = nodes_builder.freeze();
    stats.stored_nodes = node_store.len() as u64;

    // 4. Ways.
    let ways_builder = WayStoreBuilder::new();
    {
        let (wk, uw, ns, wb, sc) = (&way_keys, &used_ways, &node_store, &ways_builder, &scanned);
        let profiles = profiles.clone();
        let caps = &caps;
        let counters = &counters;
        r.phase(
            Phase::Ways,
            layout.ways.clone(),
            only(ElementType::Way),
            move |b| {
                let mut out = Vec::new();
                let mut batch = WayBatch::new();
                let mut locs: Vec<LatpLon> = Vec::new();
                for w in b.ways() {
                    let used = uw.contains(w.id());
                    let tags = w.tags();
                    if !used && !wk.filter(&tags) {
                        continue;
                    }
                    locs.clear();
                    let l = w.locations();
                    let mut missing = false;
                    if !l.is_empty() {
                        locs.extend(l.iter().map(|&x| latp_lon(x)));
                    } else {
                        for &id in w.refs() {
                            match ns.get(id) {
                                Some(x) => locs.push(latp_lon(x)),
                                None => missing = true,
                            }
                        }
                    }
                    if missing {
                        counters.missing.fetch_add(1, Ordering::Relaxed);
                    }
                    if locs.is_empty() {
                        continue;
                    }
                    if used {
                        batch.push(w.id(), locs.iter().map(|ll| (ll.latp, ll.lon)));
                    }
                    if caps.ways {
                        let obj = Object {
                            kind: ObjectKind::Way,
                            id: w.id(),
                            tags: owned_tags(&tags),
                            nodes: locs.clone(),
                            relations: if sc.is_empty() {
                                Vec::new()
                            } else {
                                sc.relations_for(MemberKind::Way, w.id())
                            },
                            ..Object::default()
                        };
                        counters.ways.fetch_add(1, Ordering::Relaxed);
                        let processed = profiles.with(|p| Ok(p.process(obj)?))?;
                        if !processed.is_empty() {
                            out.push(coverage(processed, base_zoom, Footprint::Way(&locs)));
                        }
                    }
                }
                wb.insert(batch);
                Ok(out)
            },
            |objs: Vec<IndexedObject>| {
                for o in objs {
                    data.add(o);
                }
            },
        )?;
    }
    let way_store = ways_builder.freeze();
    stats.stored_ways = way_store.len() as u64;

    // 5. Relations.
    {
        let (wk, ur, ws, ns, sc) = (&way_keys, &used_relations, &way_store, &node_store, &scanned);
        let profiles = profiles.clone();
        let caps = &caps;
        let counters = &counters;
        r.phase(
            Phase::Relations,
            layout.relations.clone(),
            only(ElementType::Relation),
            move |b| {
                let mut out = Vec::new();
                let mut buf = Vec::new();
                for rel in b.relations() {
                    let tags = rel.tags();
                    let mp = tags.get("type") == Some("multipolygon");
                    let boundary = tags.get("type") == Some("boundary");
                    if !mp && !boundary && !caps.relations {
                        continue;
                    }
                    let tags = match sc.tags(rel.id()) {
                        Some(t) => t.to_vec(),
                        None => owned_tags(&tags),
                    };
                    let tag_pairs: Vec<(&str, &str)> = tags.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                    if !ur.contains(rel.id()) && !wk_filter_pairs(wk, &tag_pairs) {
                        continue;
                    }
                    if !mp && !caps.relations {
                        continue;
                    }
                    let mut outers = Vec::new();
                    let mut inners = Vec::new();
                    let mut inner_outer = mp || boundary;
                    let mut node_members = Vec::new();
                    for m in rel.members() {
                        match m.kind {
                            ElementType::Way => {
                                if m.role == "inner" || m.role == "outer" {
                                    inner_outer = true;
                                }
                                if ws.get_into(m.id, &mut buf) {
                                    let way: Vec<LatpLon> =
                                        buf.iter().map(|&(latp, lon)| LatpLon { latp, lon }).collect();
                                    if m.role == "inner" {
                                        inners.push(way);
                                    } else {
                                        outers.push(way);
                                    }
                                }
                            }
                            ElementType::Node if !m.role.is_empty() => {
                                if let Some(loc) = ns.get(m.id) {
                                    node_members.push((m.role.to_string(), latp_lon(loc)));
                                }
                            }
                            _ => {}
                        }
                    }
                    let footprint: Vec<Vec<LatpLon>> = outers.clone();
                    let obj = Object {
                        kind: ObjectKind::Relation,
                        id: rel.id(),
                        tags,
                        outers,
                        inners,
                        closed: inner_outer,
                        multipolygon: mp,
                        node_members,
                        relations: sc.relations_for(MemberKind::Relation, rel.id()),
                        ..Object::default()
                    };
                    counters.relations.fetch_add(1, Ordering::Relaxed);
                    let processed = profiles.with(|p| Ok(p.process(obj)?))?;
                    if !processed.is_empty() {
                        out.push(coverage(processed, base_zoom, Footprint::Relation(&footprint)));
                    }
                }
                Ok(out)
            },
            |objs: Vec<IndexedObject>| {
                for o in objs {
                    data.add(o);
                }
            },
        )?;
    }
    drop(way_store);
    drop(node_store);
    // exit_function on every state.
    let mut exit_err = None;
    profiles.for_each(|p| {
        if let Err(e) = p.exit() {
            exit_err.get_or_insert(e);
        }
    });
    if let Some(e) = exit_err {
        return Err(e.into());
    }
    if let Some(e) = profiles.first_error.lock().unwrap().take() {
        return Err(CoreError::Profile(e));
    }
    data.seal();
    stats.nodes_processed = counters.nodes.load(Ordering::Relaxed);
    stats.ways_processed = counters.ways.load(Ordering::Relaxed);
    stats.relations_processed = counters.relations.load(Ordering::Relaxed);
    stats.ways_missing_nodes = counters.missing.load(Ordering::Relaxed);
    stats.objects = data.objects.len() as u64;
    Ok((data, stats))
}

fn wk_filter_pairs(wk: &SignificantTags, tags: &[(&str, &str)]) -> bool {
    wk.filter_pairs(tags)
}

#[derive(Default)]
struct Counters {
    nodes: AtomicU64,
    ways: AtomicU64,
    relations: AtomicU64,
    missing: AtomicU64,
}
