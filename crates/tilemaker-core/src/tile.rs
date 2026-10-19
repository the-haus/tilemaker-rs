//! Rendering one tile (port of `tile_worker.cpp`).

use std::cmp::Ordering;
use std::collections::HashMap;

use tilemaker_config::{Config, LayerDef};
use tilemaker_geom::clip::clip_linestring;
use tilemaker_geom::compact::{to_coords, CompactPolygons};
use tilemaker_geom::coords::{meter2degp, tiley2latp, TileCoord};
use tilemaker_geom::simplify::{simplify_linestring, simplify_multipolygon};
use tilemaker_geom::valid::{orient_multipolygon, repair_multipolygon, union_many};
use tilemaker_geom::{ll_to_coord, Coord, LineString, MultiLineString, MultiPolygon, Rect, TileBbox};
use tilemaker_lua::{AttributeValue, GeometryKind, OutputGeometry};
use tilemaker_mvt::{GeomType, LayerBuilder, TileBuilder, Value};

use crate::data::{OutputObject, TileData};

pub(crate) struct Renderer<'a> {
    pub data: &'a TileData,
    pub config: &'a Config,
    /// Tiles must intersect all of these `[west, south, east, north]` boxes
    /// (the `--bbox` or file header box, and the config's `bounding_box`).
    pub clip: Vec<[f64; 4]>,
}

impl Renderer<'_> {
    fn visible(&self, o: &OutputObject, z: u8) -> bool {
        let l = &self.config.layers.layers[o.layer as usize];
        o.min_zoom <= z && l.minzoom <= z && z <= l.maxzoom
    }

    /// The tiles at zoom `z` that hold anything, with their objects.
    pub fn zoom_tiles(&self, z: u8) -> Vec<(TileCoord, Vec<u32>)> {
        let d = self.data;
        let dz = d.base_zoom - z;
        let mut map: HashMap<TileCoord, Vec<u32>> = HashMap::new();
        for (t, objs) in &d.tiles {
            let mut visible = objs
                .iter()
                .copied()
                .filter(|&o| self.visible(&d.objects[o as usize], z))
                .peekable();
            if visible.peek().is_some() {
                map.entry(t.parent(dz)).or_default().extend(visible);
            }
        }
        for &(a, b, o) in &d.large {
            if !self.visible(&d.objects[o as usize], z) {
                continue;
            }
            let (a, b) = (a.parent(dz), b.parent(dz));
            for x in a.x..=b.x {
                for y in a.y..=b.y {
                    map.entry(TileCoord::new(x, y)).or_default().push(o);
                }
            }
        }
        let mut tiles: Vec<(TileCoord, Vec<u32>)> = map
            .into_iter()
            .filter(|(t, _)| self.in_bounds(*t, z))
            .map(|(t, mut v)| {
                v.sort_unstable();
                v.dedup();
                (t, v)
            })
            .collect();
        tiles.sort_unstable_by_key(|(t, _)| (t.x, t.y));
        tiles
    }

    fn in_bounds(&self, t: TileCoord, z: u8) -> bool {
        if self.clip.is_empty() {
            return true;
        }
        let tb = TileBbox::new(t, z, false, false);
        self.clip.iter().all(|b| tb.tile_box_intersects(b[0], b[1], b[2], b[3]))
    }

    fn compare(&self, a: u32, b: u32) -> Ordering {
        let (oa, ob) = (&self.data.objects[a as usize], &self.data.objects[b as usize]);
        oa.layer
            .cmp(&ob.layer)
            .then_with(|| {
                let asc = self.config.layers.layers[oa.layer as usize].z_order_ascending;
                let c = oa.z_order.total_cmp(&ob.z_order);
                if asc {
                    c
                } else {
                    c.reverse()
                }
            })
            .then_with(|| oa.kind.cmp(&ob.kind))
            .then_with(|| oa.attributes.cmp(&ob.attributes))
            .then_with(|| oa.geometry.cmp(&ob.geometry))
    }

    /// Render a tile; `None` when it ends up empty.
    pub fn render(&self, z: u8, t: TileCoord, mut objs: Vec<u32>) -> Option<Vec<u8>> {
        let hires = self.config.high_resolution && z == self.config.maxzoom;
        let bbox = TileBbox::new(t, z, hires, z == self.config.maxzoom);
        objs.sort_unstable_by(|&a, &b| self.compare(a, b));
        let mut tile = TileBuilder::new();
        for group in &self.config.layers.layer_order {
            let name = &self.config.layers.layers[group[0]].name;
            let layer = tile.layer(name, self.config.mvt_version, bbox.extent());
            for &l in group {
                let ld = &self.config.layers.layers[l];
                if z < ld.minzoom || z > ld.maxzoom {
                    continue;
                }
                let start = objs.partition_point(|&o| (self.data.objects[o as usize].layer as usize) < l);
                let end = objs.partition_point(|&o| (self.data.objects[o as usize].layer as usize) <= l);
                if start == end {
                    continue;
                }
                let mut list = &objs[start..end];
                if ld.feature_limit > 0 && list.len() > ld.feature_limit as usize && z < ld.feature_limit_below {
                    list = &list[..ld.feature_limit as usize];
                }
                self.layer_objects(layer, ld, list, z, t, &bbox);
            }
        }
        (!tile.is_empty()).then(|| tile.encode())
    }

    fn compatible(&self, a: u32, b: u32) -> bool {
        let (oa, ob) = (&self.data.objects[a as usize], &self.data.objects[b as usize]);
        oa.kind == ob.kind && oa.z_order == ob.z_order && oa.attributes == ob.attributes
    }

    fn layer_objects(
        &self,
        layer: &mut LayerBuilder,
        ld: &LayerDef,
        list: &[u32],
        z: u8,
        t: TileCoord,
        bbox: &TileBbox,
    ) {
        let (mut simplify, mut filter_area) = (0.0, 0.0);
        if z < ld.simplify_below || z < ld.filter_below {
            let latp = (tiley2latp(t.y, z) + tiley2latp(t.y + 1, z)) / 2.0;
            if z < ld.simplify_below {
                simplify = if ld.simplify_length > 0.0 {
                    meter2degp(ld.simplify_length, latp)
                } else {
                    ld.simplify_level
                };
                simplify *= ld.simplify_ratio.powi(ld.simplify_below as i32 - 1 - z as i32);
            }
            if z < ld.filter_below {
                filter_area = meter2degp(ld.filter_area, latp) * 2f64.powi(ld.filter_below as i32 - 1 - z as i32);
            }
        }
        let combine_lines = z < ld.combine_lines_below;
        let combine_polygons = z < ld.combine_polygons_below;
        let mut i = 0;
        while i < list.len() {
            let first = list[i];
            let o = self.data.objects[first as usize];
            // The run of compatible objects that may be merged with this one.
            let mut j = i + 1;
            let merge = match o.kind {
                GeometryKind::Point => ld.combine_points,
                GeometryKind::LineString => combine_lines,
                GeometryKind::Polygon => combine_polygons,
                GeometryKind::MultiLineString => false,
            };
            if merge {
                while j < list.len() && self.compatible(first, list[j]) {
                    j += 1;
                }
            }
            let run = &list[i..j];
            i = j;
            match o.kind {
                GeometryKind::Point => self.write_points(layer, &o, run, z, bbox),
                GeometryKind::LineString | GeometryKind::MultiLineString => {
                    let mut mls = MultiLineString::new(Vec::new());
                    for &r in run {
                        match &self.data.geometries[self.data.objects[r as usize].geometry as usize] {
                            OutputGeometry::LineString(ls) => {
                                clip_linestring(&to_coords(ls), &bbox.clipping_box, &mut mls)
                            }
                            OutputGeometry::MultiLineString(m) => {
                                for ls in m.lines() {
                                    clip_linestring(&to_coords(ls), &bbox.clipping_box, &mut mls);
                                }
                            }
                            _ => {}
                        }
                    }
                    if run.len() > 1 {
                        mls = reorder_multilinestring(mls);
                    }
                    self.write_lines(layer, &o, ld, mls, simplify, z, bbox);
                }
                GeometryKind::Polygon => {
                    let mut parts = Vec::with_capacity(run.len());
                    for &r in run {
                        if let OutputGeometry::MultiPolygon(mp) =
                            &self.data.geometries[self.data.objects[r as usize].geometry as usize]
                        {
                            let c = clip_compact(mp, &bbox.clipping_box);
                            if !c.0.is_empty() {
                                parts.push(c);
                            }
                        }
                    }
                    let mut mp = if parts.len() > 1 {
                        union_many(parts)
                    } else {
                        parts.pop().unwrap_or_else(|| MultiPolygon::new(Vec::new()))
                    };
                    if filter_area > 0.0 {
                        remove_parts_below(&mut mp, filter_area);
                    }
                    if !mp.0.is_empty() {
                        self.write_polygon(layer, &o, ld, &mp, simplify, z, bbox);
                    }
                }
            }
        }
    }

    fn write_points(&self, layer: &mut LayerBuilder, o: &OutputObject, run: &[u32], z: u8, bbox: &TileBbox) {
        let extent = bbox.extent() as i32;
        let margin = 256;
        let mut pts = Vec::with_capacity(run.len());
        for &r in run {
            if let OutputGeometry::Point(p) = &self.data.geometries[self.data.objects[r as usize].geometry as usize] {
                let (x, y) = bbox.scale_coord(ll_to_coord(*p));
                if x >= -margin && x <= extent + margin && y >= -margin && y <= extent + margin {
                    pts.push((x, y));
                }
            }
        }
        if pts.is_empty() {
            return;
        }
        let mut f = layer.feature(GeomType::Point);
        self.feature_header(&mut f, o, z);
        f.add_points(&pts);
        f.commit();
    }

    #[allow(clippy::too_many_arguments)]
    fn write_lines(
        &self,
        layer: &mut LayerBuilder,
        o: &OutputObject,
        ld: &LayerDef,
        mls: MultiLineString,
        simplify: f64,
        z: u8,
        bbox: &TileBbox,
    ) {
        let mut f = layer.feature(GeomType::LineString);
        let mut any = false;
        let mut pts: Vec<(i32, i32)> = Vec::new();
        for ls in mls.0 {
            let ls = if simplify > 0.0 {
                simplify_linestring(&ls, simplify, ld.simplify_algorithm)
            } else {
                ls
            };
            pts.clear();
            for c in &ls.0 {
                let p = bbox.scale_coord(*c);
                if pts.last() != Some(&p) {
                    pts.push(p);
                }
            }
            if pts.len() >= 2 {
                f.add_linestring(&pts);
                any = true;
            }
        }
        if any {
            self.feature_header(&mut f, o, z);
            f.commit();
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_polygon(
        &self,
        layer: &mut LayerBuilder,
        o: &OutputObject,
        ld: &LayerDef,
        mp: &MultiPolygon,
        simplify: f64,
        z: u8,
        bbox: &TileBbox,
    ) {
        let mut scaled = bbox.scale_multipolygon(mp);
        if simplify > 0.0 {
            scaled = simplify_multipolygon(&scaled, simplify / bbox.xscale, ld.simplify_algorithm);
        }
        if scaled.0.is_empty() {
            return;
        }
        // In tile space (y down) the default orientation gives exteriors a
        // positive area, which is what MVT calls clockwise.
        orient_multipolygon(&mut scaled);
        if simplify > 0.0 && !tilemaker_geom::valid::is_valid(&scaled) {
            repair_multipolygon(&mut scaled);
            orient_multipolygon(&mut scaled);
        }
        let mut f = layer.feature(GeomType::Polygon);
        let mut any = false;
        let ring = |r: &LineString| -> Vec<(i32, i32)> {
            let mut v: Vec<(i32, i32)> = Vec::with_capacity(r.0.len());
            for c in &r.0 {
                let p = (c.x.round() as i32, c.y.round() as i32);
                if v.last() != Some(&p) {
                    v.push(p);
                }
            }
            v
        };
        for p in &scaled.0 {
            let ext = ring(p.exterior());
            if distinct(&ext) < 3 {
                continue;
            }
            f.add_ring(&ext);
            any = true;
            for hole in p.interiors() {
                let h = ring(hole);
                if distinct(&h) >= 3 {
                    f.add_ring(&h);
                }
            }
        }
        if any {
            self.feature_header(&mut f, o, z);
            f.commit();
        }
    }

    fn feature_header(&self, f: &mut tilemaker_mvt::FeatureBuilder<'_>, o: &OutputObject, z: u8) {
        if self.config.include_ids && o.id > 0 {
            f.set_id(o.id as u64);
        }
        for a in self.data.attribute_sets[o.attributes as usize].iter() {
            if a.min_zoom > z {
                continue;
            }
            let v = match &a.value {
                AttributeValue::String(s) => Value::String(s.clone()),
                AttributeValue::Float(n) => Value::Float(*n as f32),
                AttributeValue::Int(i) => Value::Int(*i),
                AttributeValue::Bool(b) => Value::Bool(*b),
            };
            f.add_property(&a.key, v);
        }
    }
}

/// Clip compact polygons to `r`: only polygons whose exterior's box meets
/// `r` are converted, and those wholly inside are not clipped.
fn clip_compact(cp: &CompactPolygons, r: &Rect) -> MultiPolygon {
    let to_e7 = |v: f64| (v * 1e7).round() as i64;
    let (rx0, ry0, rx1, ry1) = (to_e7(r.min().x), to_e7(r.min().y), to_e7(r.max().x), to_e7(r.max().y));
    let mut out = Vec::new();
    for (rings, b) in cp.polygons().zip(cp.boxes.iter()) {
        let ext = rings[0];
        let (x0, y0, x1, y1) = (b[0] as i64, b[1] as i64, b[2] as i64, b[3] as i64);
        if x1 < rx0 || x0 > rx1 || y1 < ry0 || y0 > ry1 {
            continue;
        }
        let inside = x0 >= rx0 && x1 <= rx1 && y0 >= ry0 && y1 <= ry1;
        let ring = |lls: &[tilemaker_geom::LatpLon]| -> Vec<Coord> {
            let c = to_coords(lls);
            if inside {
                c
            } else {
                tilemaker_geom::clip::clip_ring(&c, r)
            }
        };
        let exterior = ring(ext);
        if exterior.len() < 4 {
            continue;
        }
        let holes = rings[1..]
            .iter()
            .map(|h| ring(h))
            .filter(|h| h.len() >= 4)
            .map(LineString::new)
            .collect();
        out.push(geo::Polygon::new(LineString::new(exterior), holes));
    }
    MultiPolygon::new(out)
}

fn distinct(ring: &[(i32, i32)]) -> usize {
    let n = ring.len();
    if n > 1 && ring[0] == ring[n - 1] {
        n - 1
    } else {
        n
    }
}

/// Drop polygons and holes smaller than `area` (in projected degrees²).
fn remove_parts_below(mp: &mut MultiPolygon, area: f64) {
    use geo::Area;
    mp.0.retain(|p| p.unsigned_area() >= area);
    for p in mp.0.iter_mut() {
        if p.interiors()
            .iter()
            .all(|h| geo::Polygon::new(h.clone(), vec![]).unsigned_area() >= area)
        {
            continue;
        }
        let kept: Vec<LineString> = p
            .interiors()
            .iter()
            .filter(|h| geo::Polygon::new((*h).clone(), vec![]).unsigned_area() >= area)
            .cloned()
            .collect();
        *p = geo::Polygon::new(p.exterior().clone(), kept);
    }
}

/// Join lines that continue one another (port of `ReorderMultiLinestring`).
fn reorder_multilinestring(input: MultiLineString) -> MultiLineString {
    let key = |c: Coord| (c.x.to_bits(), c.y.to_bits());
    let lines: Vec<LineString> = input.0.into_iter().filter(|l| l.0.len() >= 2).collect();
    let mut starts: HashMap<(u64, u64), usize> = HashMap::new();
    let mut ends: HashMap<(u64, u64), usize> = HashMap::new();
    for (i, l) in lines.iter().enumerate() {
        starts.insert(key(l.0[0]), i);
        ends.insert(key(*l.0.last().unwrap()), i);
    }
    let mut added = vec![false; lines.len()];
    let mut out = Vec::new();
    for i in 0..lines.len() {
        if added[i] {
            continue;
        }
        added[i] = true;
        let mut ls = lines[i].0.clone();
        loop {
            if let Some(&j) = starts.get(&key(*ls.last().unwrap())) {
                if !added[j] && lines[j].0.len() + ls.len() < 6000 {
                    ls.extend_from_slice(&lines[j].0[1..]);
                    added[j] = true;
                    continue;
                }
            }
            if let Some(&j) = ends.get(&key(ls[0])) {
                if !added[j] && lines[j].0.len() + ls.len() < 6000 {
                    let mut v = lines[j].0[..lines[j].0.len() - 1].to_vec();
                    v.extend_from_slice(&ls);
                    ls = v;
                    added[j] = true;
                    continue;
                }
            }
            break;
        }
        out.push(LineString::new(ls));
    }
    MultiLineString::new(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_clipping_matches_geo_clipping() {
        use geo::Area;
        let c = |x: f64, y: f64| Coord { x, y };
        // A star with a hole, and a second square.
        let star: Vec<Coord> = (0..=40)
            .map(|i| {
                let t = (i % 40) as f64 / 40.0 * std::f64::consts::TAU;
                let r = if i % 2 == 0 { 1.0 } else { 0.5 };
                c(r * t.cos(), r * t.sin())
            })
            .collect();
        let hole: Vec<Coord> = vec![c(-0.1, -0.1), c(-0.1, 0.1), c(0.1, 0.1), c(0.1, -0.1), c(-0.1, -0.1)];
        let mut mp = MultiPolygon::new(vec![
            geo::Polygon::new(LineString::new(star), vec![LineString::new(hole)]),
            geo::Polygon::new(
                LineString::new(vec![c(3.0, 3.0), c(4.0, 3.0), c(4.0, 4.0), c(3.0, 4.0), c(3.0, 3.0)]),
                vec![],
            ),
        ]);
        tilemaker_geom::valid::orient_multipolygon(&mut mp);
        let cp = CompactPolygons::from_geo(&mp);
        let mut seed = 7u64;
        let mut rnd = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 11) as f64 / (1u64 << 53) as f64 * 6.0 - 1.5
        };
        for _ in 0..200 {
            let (a, b, d, e) = (rnd(), rnd(), rnd(), rnd());
            let r = Rect::new(c(a.min(d), b.min(e)), c(a.max(d), b.max(e)));
            let ours = clip_compact(&cp, &r);
            let theirs = tilemaker_geom::clip::clip_multipolygon(&cp.to_geo(), &r);
            assert!((ours.unsigned_area() - theirs.unsigned_area()).abs() < 1e-9, "{r:?}");
            assert_eq!(ours.0.len(), theirs.0.len(), "{r:?}");
        }
    }

    #[test]
    fn reorders_lines() {
        let c = |x: f64| Coord { x, y: 0.0 };
        let mls = MultiLineString::new(vec![
            LineString::new(vec![c(2.0), c(3.0)]),
            LineString::new(vec![c(0.0), c(1.0)]),
            LineString::new(vec![c(1.0), c(2.0)]),
            LineString::new(vec![c(10.0), c(11.0)]),
        ]);
        let out = reorder_multilinestring(mls);
        assert_eq!(out.0.len(), 2);
        assert_eq!(out.0[0].0, vec![c(0.0), c(1.0), c(2.0), c(3.0)]);
    }
}
