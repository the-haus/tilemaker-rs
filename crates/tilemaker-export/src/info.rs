//! What a PBF file holds (like `osmium fileinfo -e`): its header, and with
//! a full read the element counts, id ranges, data bounding box,
//! timestamps, ordering and metadata.

use std::collections::BTreeMap;

use tilemaker_pbf::{DecodeOptions, Header, PbfFile, PrimitiveBlock};

use crate::engine::{Flow, Phase, Progress};
use crate::ExportError;

/// Per element type (nodes, ways, relations).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct PerType<T> {
    /// Nodes.
    pub nodes: T,
    /// Ways.
    pub ways: T,
    /// Relations.
    pub relations: T,
}

impl<T> PerType<T> {
    fn get_mut(&mut self, t: usize) -> &mut T {
        match t {
            0 => &mut self.nodes,
            1 => &mut self.ways,
            _ => &mut self.relations,
        }
    }
}

/// Metadata attributes: which objects have them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct MetadataInfo {
    /// Attributes every object has (of `version`, `timestamp`,
    /// `changeset`, `uid`, `user`).
    pub all: Vec<String>,
    /// Attributes some objects have.
    pub some: Vec<String>,
}

/// What a full read finds.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataInfo {
    /// Element counts.
    pub counts: PerType<u64>,
    /// Elements with at least one tag.
    pub tagged: PerType<u64>,
    /// Smallest ids (`None` when there are no elements of the type).
    pub min_id: PerType<Option<i64>>,
    /// Largest ids.
    pub max_id: PerType<Option<i64>>,
    /// Node references of all ways.
    pub way_nodes: u64,
    /// Members of all relations.
    pub relation_members: u64,
    /// Bounding box of the nodes `[west, south, east, north]`.
    pub bbox: Option<[f64; 4]>,
    /// Earliest and latest timestamps (seconds since the epoch), when the
    /// file has them.
    pub timestamps: Option<[i64; 2]>,
    /// Whether elements are ordered by type, then id.
    pub objects_ordered: bool,
    /// Whether an element appears more than once in a row (history files).
    pub multiple_versions: bool,
    /// Metadata attributes present.
    pub metadata: MetadataInfo,
}

/// What a file holds.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    /// The path.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Data blocks.
    pub blocks: usize,
    /// Blocks per compression (`zlib`, `zstd`, `lz4`, `none`, …).
    pub compression: BTreeMap<String, usize>,
    /// The header.
    #[serde(skip)]
    pub header: Header,
    /// With a full read: what the data holds.
    pub data: Option<DataInfo>,
}

impl FileInfo {
    /// As JSON, with the header (camelCase keys, as the Node API).
    pub fn to_json(&self) -> serde_json::Value {
        let h = &self.header;
        let mut v = serde_json::to_value(self).unwrap_or_default();
        v["header"] = serde_json::json!({
            "bbox": h.bbox.map(|b| [b.min_lon, b.min_lat, b.max_lon, b.max_lat]),
            "requiredFeatures": h.required_features,
            "optionalFeatures": h.optional_features,
            "writingProgram": h.writing_program,
            "source": h.source,
            "replicationTimestamp": h.replication_timestamp,
            "replicationSequenceNumber": h.replication_sequence_number,
            "replicationBaseUrl": h.replication_base_url,
            "sorted": h.is_sorted(),
            "locationsOnWays": h.has_locations_on_ways(),
        });
        v
    }
}

/// One block's share of [`DataInfo`].
#[derive(Default)]
struct Summary {
    counts: [u64; 3],
    tagged: [u64; 3],
    min_id: [Option<i64>; 3],
    max_id: [Option<i64>; 3],
    first: Option<(u8, i64)>,
    last: Option<(u8, i64)>,
    ordered: bool,
    repeated: bool,
    bbox: Option<[i32; 4]>,
    timestamps: Option<[i64; 2]>,
    way_nodes: u64,
    relation_members: u64,
    /// Objects having each metadata attribute.
    meta: [u64; 5],
}

const ATTRIBUTES: [&str; 5] = ["version", "timestamp", "changeset", "uid", "user"];

impl Summary {
    fn of(b: &PrimitiveBlock) -> Self {
        let mut s = Summary {
            ordered: true,
            ..Default::default()
        };
        let see = |s: &mut Summary, t: u8, id: i64, tagged: bool, info: Option<tilemaker_pbf::InfoRef<'_>>| {
            let i = t as usize;
            s.counts[i] += 1;
            s.tagged[i] += tagged as u64;
            s.min_id[i] = Some(s.min_id[i].map_or(id, |m| m.min(id)));
            s.max_id[i] = Some(s.max_id[i].map_or(id, |m| m.max(id)));
            if let Some(prev) = s.last {
                if prev == (t, id) {
                    s.repeated = true;
                } else if prev > (t, id) {
                    s.ordered = false;
                }
            }
            s.first.get_or_insert((t, id));
            s.last = Some((t, id));
            if let Some(info) = info {
                let has = [
                    info.version > 0,
                    info.timestamp > 0,
                    info.changeset > 0,
                    info.uid > 0,
                    !info.user().is_empty(),
                ];
                for (k, h) in has.into_iter().enumerate() {
                    s.meta[k] += h as u64;
                }
                if info.timestamp > 0 {
                    let ts = s.timestamps.get_or_insert([info.timestamp, info.timestamp]);
                    ts[0] = ts[0].min(info.timestamp);
                    ts[1] = ts[1].max(info.timestamp);
                }
            }
        };
        for n in b.nodes() {
            see(&mut s, 0, n.id(), !n.tags().is_empty(), n.info());
            let (lon, lat) = (n.lon_e7(), n.lat_e7());
            let bb = s.bbox.get_or_insert([lon, lat, lon, lat]);
            *bb = [bb[0].min(lon), bb[1].min(lat), bb[2].max(lon), bb[3].max(lat)];
        }
        for w in b.ways() {
            see(&mut s, 1, w.id(), !w.tags().is_empty(), w.info());
            s.way_nodes += w.refs().len() as u64;
        }
        for r in b.relations() {
            see(&mut s, 2, r.id(), !r.tags().is_empty(), r.info());
            s.relation_members += r.member_count() as u64;
        }
        s
    }
}

/// The file's header, size, blocks and their compression; with `full`,
/// also read every element (on `threads` workers) for [`DataInfo`].
/// `progress` sees each block and may stop the read (the data is then
/// what was read so far).
pub fn file_info(
    file: &PbfFile,
    full: bool,
    threads: usize,
    mut progress: impl FnMut(Progress) -> Flow,
) -> Result<FileInfo, ExportError> {
    let mut compression = BTreeMap::new();
    for b in file.blobs() {
        *compression.entry(file.blob_compression(b)?.to_string()).or_insert(0) += 1;
    }
    let mut info = FileInfo {
        path: file.path().display().to_string(),
        size: file.len(),
        blocks: file.blobs().len(),
        compression,
        header: file.header().clone(),
        data: None,
    };
    if !full {
        return Ok(info);
    }
    let threads = if threads == 0 {
        tilemaker_pbf::parallel::default_threads()
    } else {
        threads
    };
    let total = file.blobs().len();
    let mut d = DataInfo {
        objects_ordered: true,
        ..Default::default()
    };
    let mut bbox: Option<[i32; 4]> = None;
    let mut meta = [0u64; 5];
    let mut last: Option<(u8, i64)> = None;
    let blocks = file.map_blocks(0..total, DecodeOptions::WITH_METADATA, threads, |_, b| {
        Ok(Summary::of(&b))
    });
    for (done, s) in blocks.enumerate() {
        let s = s?;
        for t in 0..3 {
            *d.counts.get_mut(t) += s.counts[t];
            *d.tagged.get_mut(t) += s.tagged[t];
            if let Some(m) = s.min_id[t] {
                let v = d.min_id.get_mut(t);
                *v = Some(v.map_or(m, |x| x.min(m)));
            }
            if let Some(m) = s.max_id[t] {
                let v = d.max_id.get_mut(t);
                *v = Some(v.map_or(m, |x| x.max(m)));
            }
        }
        d.way_nodes += s.way_nodes;
        d.relation_members += s.relation_members;
        d.objects_ordered &= s.ordered;
        d.multiple_versions |= s.repeated;
        if let (Some(prev), Some(first)) = (last, s.first) {
            if prev == first {
                d.multiple_versions = true;
            } else if prev > first {
                d.objects_ordered = false;
            }
        }
        if s.last.is_some() {
            last = s.last;
        }
        if let Some(b) = s.bbox {
            let bb = bbox.get_or_insert(b);
            *bb = [bb[0].min(b[0]), bb[1].min(b[1]), bb[2].max(b[2]), bb[3].max(b[3])];
        }
        if let Some(t) = s.timestamps {
            let ts = d.timestamps.get_or_insert(t);
            *ts = [ts[0].min(t[0]), ts[1].max(t[1])];
        }
        for (m, n) in meta.iter_mut().zip(s.meta) {
            *m += n;
        }
        let p = Progress {
            phase: Phase::Elements,
            blocks_done: done + 1,
            blocks_total: total,
        };
        if progress(p) == Flow::Stop {
            break;
        }
    }
    d.bbox = bbox.map(|b| b.map(|v| v as f64 / 1e7));
    let objects = d.counts.nodes + d.counts.ways + d.counts.relations;
    for (k, name) in ATTRIBUTES.iter().enumerate() {
        if objects > 0 && meta[k] == objects {
            d.metadata.all.push(name.to_string());
        }
        if meta[k] > 0 {
            d.metadata.some.push(name.to_string());
        }
    }
    info.data = Some(d);
    Ok(info)
}
