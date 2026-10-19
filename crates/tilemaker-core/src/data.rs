//! Everything read from the OSM file that tiles are made of: output
//! objects, their geometries and attribute sets, and the tile index.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use tilemaker_geom::coords::{latp2tileyf, lon2tilexf, TileCoord, MAX_LATP, MIN_LATP};
use tilemaker_geom::tiles::{fill_covered_tiles, insert_intermediate_tiles};
use tilemaker_geom::{ll_to_coord, Coord, LatpLon};
use tilemaker_lua::{Attribute, GeometryKind, OutputGeometry, ProcessedObject};

/// One `Layer()` output, ready for tiles.
#[derive(Clone, Copy, Debug)]
pub struct OutputObject {
    /// Layer index.
    pub layer: u8,
    /// Geometry kind.
    pub kind: GeometryKind,
    /// Minimum zoom.
    pub min_zoom: u8,
    /// `ZOrder()`.
    pub z_order: f32,
    /// Attribute set index.
    pub attributes: u32,
    /// Geometry index.
    pub geometry: u32,
    /// OSM id (for `include_ids`).
    pub id: i64,
}

/// Where an object goes in the index.
#[derive(Clone, Debug)]
pub enum Coverage {
    /// These base-zoom tiles.
    Tiles(Vec<TileCoord>),
    /// Every base-zoom tile in `[min, max]` (large polygons).
    Rect(TileCoord, TileCoord),
}

/// A processed object with its coverage, as produced by a worker.
pub struct IndexedObject {
    /// The profile's outputs and geometries.
    pub object: ProcessedObject,
    /// Coverage per geometry.
    pub coverage: Vec<Coverage>,
}

/// Polygons covering this many base tiles or more go to the rectangle list
/// rather than into every tile (as tilemaker's "large index").
const LARGE: u64 = 16;

/// What an object covers, for points it labels: tilemaker indexes a
/// `LayerAsCentroid` point of a way or relation in every tile the way or
/// relation touches (each tile then keeps it if it falls within the tile's
/// buffer), so labels near tile edges also reach the neighbouring tiles.
pub enum Footprint<'a> {
    /// A node: points cover their own tile.
    Node,
    /// A way's node locations.
    Way(&'a [LatpLon]),
    /// A relation's member ways.
    Relation(&'a [Vec<LatpLon>]),
}

/// Compute which base-zoom tiles each geometry of an object touches.
pub fn coverage(object: ProcessedObject, base_zoom: u8, footprint: Footprint<'_>) -> IndexedObject {
    let mut labels: Option<Coverage> = None;
    let coverage = object
        .geometries
        .iter()
        .map(|g| match (g, &footprint) {
            (OutputGeometry::Point(_), Footprint::Way(nodes)) => labels
                .get_or_insert_with(|| {
                    let mut set = HashSet::new();
                    let line: Vec<Coord> = nodes.iter().map(|&n| ll_to_coord(n)).collect();
                    insert_intermediate_tiles(&line, base_zoom, &mut set);
                    let mut v: Vec<TileCoord> = set.into_iter().collect();
                    v.sort_unstable();
                    Coverage::Tiles(v)
                })
                .clone(),
            (OutputGeometry::Point(_), Footprint::Relation(ways)) => labels
                .get_or_insert_with(|| {
                    let mut set = HashSet::new();
                    for w in ways.iter() {
                        let line: Vec<Coord> = w.iter().map(|&n| ll_to_coord(n)).collect();
                        insert_intermediate_tiles(&line, base_zoom, &mut set);
                    }
                    if set.is_empty() {
                        return Coverage::Tiles(Vec::new());
                    }
                    let (min, max) = bounds(&set);
                    if (max.x - min.x + 1) as u64 * (max.y - min.y + 1) as u64 >= LARGE {
                        return Coverage::Rect(min, max);
                    }
                    fill_covered_tiles(&mut set);
                    let mut v: Vec<TileCoord> = set.into_iter().collect();
                    v.sort_unstable();
                    Coverage::Tiles(v)
                })
                .clone(),
            _ => cover(g, base_zoom),
        })
        .collect();
    IndexedObject { object, coverage }
}

fn cover(g: &OutputGeometry, z: u8) -> Coverage {
    let mut set = HashSet::new();
    match g {
        OutputGeometry::Point(p) => {
            let c = ll_to_coord(*p);
            return Coverage::Tiles(vec![tile_of(c, z)]);
        }
        OutputGeometry::LineString(ls) => insert_intermediate_tiles(&ls.0, z, &mut set),
        OutputGeometry::MultiLineString(mls) => {
            for ls in &mls.0 {
                insert_intermediate_tiles(&ls.0, z, &mut set);
            }
        }
        OutputGeometry::MultiPolygon(mp) => {
            let mut all = HashSet::new();
            for p in &mp.0 {
                let mut s = HashSet::new();
                insert_intermediate_tiles(&p.exterior().0, z, &mut s);
                let (min, max) = bounds(&s);
                if (max.x - min.x + 1) as u64 * (max.y - min.y + 1) as u64 >= LARGE {
                    // Large: index by rectangle, all parts together.
                    let (mut min, mut max) = (min, max);
                    for q in &mp.0 {
                        let mut t = HashSet::new();
                        insert_intermediate_tiles(&q.exterior().0, z, &mut t);
                        let (a, b) = bounds(&t);
                        min = TileCoord::new(min.x.min(a.x), min.y.min(a.y));
                        max = TileCoord::new(max.x.max(b.x), max.y.max(b.y));
                    }
                    return Coverage::Rect(min, max);
                }
                fill_covered_tiles(&mut s);
                all.extend(s);
            }
            set = all;
        }
    }
    let mut v: Vec<TileCoord> = set.into_iter().collect();
    v.sort_unstable();
    Coverage::Tiles(v)
}

fn bounds(s: &HashSet<TileCoord>) -> (TileCoord, TileCoord) {
    let mut min = TileCoord::new(u32::MAX, u32::MAX);
    let mut max = TileCoord::new(0, 0);
    for t in s {
        min = TileCoord::new(min.x.min(t.x), min.y.min(t.y));
        max = TileCoord::new(max.x.max(t.x), max.y.max(t.y));
    }
    (min, max)
}

fn tile_of(c: Coord, z: u8) -> TileCoord {
    let max = ((1u64 << z) - 1) as f64;
    TileCoord::new(
        lon2tilexf(c.x, z).clamp(0.0, max) as u32,
        latp2tileyf(c.y.clamp(MIN_LATP, MAX_LATP), z).clamp(0.0, max) as u32,
    )
}

/// The tile data.
#[derive(Default)]
pub struct TileData {
    /// Base zoom of the index.
    pub base_zoom: u8,
    /// Output objects.
    pub objects: Vec<OutputObject>,
    /// Geometries.
    pub geometries: Vec<OutputGeometry>,
    /// Distinct attribute sets (sorted by key).
    pub attribute_sets: Vec<Arc<[Attribute]>>,
    attribute_index: HashMap<Arc<[Attribute]>, u32>,
    /// Base-zoom tile → objects.
    pub tiles: HashMap<TileCoord, Vec<u32>>,
    /// Large objects: base-zoom tile rectangle → object.
    pub large: Vec<(TileCoord, TileCoord, u32)>,
}

impl TileData {
    /// Empty data indexed at `base_zoom`.
    pub fn new(base_zoom: u8) -> Self {
        Self {
            base_zoom,
            ..Default::default()
        }
    }

    fn attribute_set(&mut self, mut attrs: Vec<Attribute>) -> u32 {
        attrs.sort_by(|a, b| a.key.cmp(&b.key));
        let attrs: Arc<[Attribute]> = attrs.into();
        if let Some(&i) = self.attribute_index.get(&attrs) {
            return i;
        }
        let i = self.attribute_sets.len() as u32;
        self.attribute_sets.push(attrs.clone());
        self.attribute_index.insert(attrs, i);
        i
    }

    /// Add one indexed object.
    pub fn add(&mut self, o: IndexedObject) {
        let IndexedObject { object, coverage } = o;
        let base = self.geometries.len() as u32;
        let ProcessedObject {
            id,
            geometries,
            outputs,
        } = object;
        self.geometries.extend(geometries);
        for out in outputs {
            if out.layer == u8::MAX {
                continue;
            }
            let attributes = self.attribute_set(out.attributes);
            let geometry = base + out.geometry as u32;
            let obj = self.objects.len() as u32;
            self.objects.push(OutputObject {
                layer: out.layer,
                kind: self.geometries[geometry as usize].kind(),
                min_zoom: out.min_zoom,
                z_order: out.z_order,
                attributes,
                geometry,
                id,
            });
            match &coverage[out.geometry] {
                Coverage::Tiles(tiles) => {
                    for t in tiles {
                        self.tiles.entry(*t).or_default().push(obj);
                    }
                }
                Coverage::Rect(a, b) => self.large.push((*a, *b, obj)),
            }
        }
    }

    /// Release memory only needed while adding.
    pub fn seal(&mut self) {
        self.attribute_index = HashMap::new();
        self.objects.shrink_to_fit();
        self.geometries.shrink_to_fit();
        for v in self.tiles.values_mut() {
            v.shrink_to_fit();
        }
    }
}
