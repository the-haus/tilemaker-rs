//! Building rings and multipolygons from member ways.
//!
//! [`merge_ways`] and [`multipolygon_from_roles`] are ports of tilemaker's
//! `mergeMultiPolygonWays` and `wayListMultiPolygon`: inner/outer roles
//! decide what is a hole. [`assemble_area`] instead nests the rings by
//! containment, ignoring roles, like osmium's area assembler; it is what the
//! feature exporter uses, as many real relations carry missing or wrong
//! roles.

use std::collections::HashMap;

use crate::coords::LatpLon;
use crate::valid::orient_multipolygon;
use crate::{ll_to_coord, LineString, MultiLineString, MultiPolygon, Polygon};

/// Join ways that share endpoints into longer runs. Closed ways are kept as
/// they are; ways that join nothing stay separate (and open).
pub fn merge_ways(ways: &[&[LatpLon]]) -> Vec<Vec<LatpLon>> {
    let mut results: Vec<Vec<LatpLon>> = Vec::new();
    let mut done = vec![false; ways.len()];
    let mut starts: HashMap<LatpLon, Vec<usize>> = HashMap::new();
    let mut ends: HashMap<LatpLon, Vec<usize>> = HashMap::new();

    for (i, way) in ways.iter().enumerate() {
        if way.len() < 2 {
            done[i] = true;
            continue;
        }
        if way.first() == way.last() || results.is_empty() {
            results.push(way.to_vec());
            done[i] = true;
        } else {
            starts.entry(way[0]).or_default().push(i);
            ends.entry(*way.last().unwrap()).or_default().push(i);
        }
    }

    let remove = |w: usize,
                  starts: &mut HashMap<LatpLon, Vec<usize>>,
                  ends: &mut HashMap<LatpLon, Vec<usize>>,
                  done: &mut Vec<bool>| {
        let way = ways[w];
        for key in [way[0], *way.last().unwrap()] {
            for map in [&mut *starts, &mut *ends] {
                if let Some(list) = map.get_mut(&key) {
                    list.retain(|&x| x != w);
                    if list.is_empty() {
                        map.remove(&key);
                    }
                }
            }
        }
        done[w] = true;
    };

    loop {
        let mut added = 0;
        // Indexing: each run is extended or replaced in place.
        #[allow(clippy::needless_range_loop)]
        for r in 0..results.len() {
            loop {
                let first = results[r][0];
                let last = *results[r].last().unwrap();
                if first == last {
                    break;
                }
                if let Some(&w) = starts.get(&last).and_then(|l| l.last()) {
                    results[r].extend_from_slice(&ways[w][1..]);
                    remove(w, &mut starts, &mut ends, &mut done);
                } else if let Some(&w) = ends.get(&last).and_then(|l| l.last()) {
                    results[r].extend(ways[w].iter().rev().skip(1));
                    remove(w, &mut starts, &mut ends, &mut done);
                } else if let Some(&w) = ends.get(&first).and_then(|l| l.last()) {
                    let mut v = ways[w][..ways[w].len() - 1].to_vec();
                    v.extend_from_slice(&results[r]);
                    results[r] = v;
                    remove(w, &mut starts, &mut ends, &mut done);
                } else if let Some(&w) = starts.get(&first).and_then(|l| l.last()) {
                    let mut v: Vec<LatpLon> = ways[w].iter().rev().copied().collect();
                    v.pop();
                    v.extend_from_slice(&results[r]);
                    results[r] = v;
                    remove(w, &mut starts, &mut ends, &mut done);
                } else {
                    break;
                }
                added += 1;
            }
        }
        if added == 0 {
            // Seed a new run with the lowest-numbered way left over.
            let next = (0..ways.len()).find(|&i| !done[i]);
            match next {
                Some(w) => {
                    results.push(ways[w].to_vec());
                    remove(w, &mut starts, &mut ends, &mut done);
                }
                None => break,
            }
        }
    }
    results
}

fn ring_of(lls: &[LatpLon]) -> LineString {
    LineString::new(lls.iter().map(|&ll| ll_to_coord(ll)).collect())
}

/// tilemaker's multipolygon: merge outers and inners separately, put each
/// inner in the outer that contains it (or the only outer), fix winding.
pub fn multipolygon_from_roles(outers: &[&[LatpLon]], inners: &[&[LatpLon]]) -> MultiPolygon {
    if outers.is_empty() {
        return MultiPolygon::new(Vec::new());
    }
    let outer_rings = merge_ways(outers);
    let inner_rings: Vec<LineString> = merge_ways(inners).iter().map(|r| ring_of(r)).collect();
    let only_one = outer_rings.len() == 1;
    let mut polys = Vec::with_capacity(outer_rings.len());
    for o in &outer_rings {
        let outer = ring_of(o);
        let outer_poly = Polygon::new(outer.clone(), vec![]);
        let holes = inner_rings
            .iter()
            .filter(|i| only_one || ring_within(i, &outer_poly))
            .cloned()
            .collect();
        polys.push(Polygon::new(outer, holes));
    }
    let mut mp = MultiPolygon::new(polys);
    close_rings(&mut mp);
    orient_multipolygon(&mut mp);
    mp
}

/// A multi-linestring from member ways, joined where they share endpoints.
pub fn multilinestring_from_ways(ways: &[&[LatpLon]]) -> MultiLineString {
    MultiLineString::new(merge_ways(ways).iter().map(|r| ring_of(r)).collect())
}

fn close_rings(mp: &mut MultiPolygon) {
    for p in mp.0.iter_mut() {
        p.exterior_mut(|r| r.close());
        p.interiors_mut(|rs| rs.iter_mut().for_each(|r| r.close()));
    }
}

/// Whether ring `inner` lies within `outer` (tested on its vertices, so
/// rings that touch at a vertex still count).
fn ring_within(inner: &LineString, outer: &Polygon) -> bool {
    point_votes(&inner.0, &outer.exterior().0) > 0
}

/// Positive when most of `pts` that are not on `ring` are inside it.
fn point_votes(pts: &[crate::Coord], ring: &[crate::Coord]) -> i32 {
    let mut votes = 0;
    let mut counted = 0;
    // Sample a few spread-out vertices; enough to outvote shared ones.
    let step = (pts.len() / 7).max(1);
    for c in pts.iter().step_by(step) {
        match point_in_ring(*c, ring) {
            Some(true) => votes += 1,
            Some(false) => votes -= 1,
            None => continue,
        }
        counted += 1;
        if counted >= 7 {
            break;
        }
    }
    votes
}

/// Even–odd point-in-ring; `None` when the point is on the boundary.
pub fn point_in_ring(p: crate::Coord, ring: &[crate::Coord]) -> Option<bool> {
    let n = ring.len();
    if n < 3 {
        return Some(false);
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (ring[i], ring[j]);
        if a == p {
            return None;
        }
        if (a.y > p.y) != (b.y > p.y) {
            let x = (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x;
            if x == p.x {
                return None;
            }
            if p.x < x {
                inside = !inside;
            }
        } else if a.y == p.y && b.y == p.y && p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) {
            return None;
        }
        j = i;
    }
    Some(inside)
}

/// The result of [`assemble_area`].
#[derive(Clone, Debug)]
pub struct Assembled {
    /// The area, with exteriors counter-clockwise and holes clockwise.
    pub multipolygon: MultiPolygon,
    /// Runs of member ways that did not close into rings (ignored).
    pub open_rings: usize,
    /// Whether segments cross (other than at shared endpoints): the area is
    /// then invalid and `multipolygon` is empty.
    pub crossing: bool,
}

/// Assemble an area from all its member ways, as osmium's area assembler
/// does:
///
/// 1. Split every way into segments; a segment that occurs an even number
///    of times cancels out (two adjacent rings sharing an edge become one).
/// 2. Walk the remaining segments into rings, splitting off a sub-ring
///    whenever a location repeats, so every ring is simple (rings that
///    touch at a point stay separate rings).
/// 3. Nest the rings by containment: inside an even number of others is an
///    exterior, inside an odd number a hole. Roles are not consulted.
///
/// Segments that do not close into rings are counted in `open_rings`.
pub fn assemble_area(ways: &[&[LatpLon]]) -> Assembled {
    let (rings, open_rings) = build_rings(ways);
    let segments: Vec<(LatpLon, LatpLon)> = rings.iter().flat_map(|r| r.windows(2).map(|w| (w[0], w[1]))).collect();
    if has_crossings(&segments) {
        return Assembled {
            multipolygon: MultiPolygon::new(Vec::new()),
            open_rings,
            crossing: true,
        };
    }
    let mut rings: Vec<(LineString, f64, geo::Rect<f64>)> = rings
        .into_iter()
        .filter_map(|r| {
            let ls = ring_of(&r);
            let area = signed_ring_area(&ls.0).abs();
            if area == 0.0 {
                return None;
            }
            let bbox = geo::BoundingRect::bounding_rect(&ls)?;
            Some((ls, area, bbox))
        })
        .collect();
    // Largest first: a ring can only be inside one that is bigger, so its
    // candidates' depths are known when it is placed.
    rings.sort_by(|a, b| b.1.total_cmp(&a.1));
    let n = rings.len();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    let mut depth = vec![0usize; n];
    if n > 1 {
        let tree = rstar::RTree::bulk_load(
            rings
                .iter()
                .enumerate()
                .map(|(i, r)| {
                    rstar::primitives::GeomWithData::new(
                        rstar::primitives::Rectangle::from_corners(
                            [r.2.min().x, r.2.min().y],
                            [r.2.max().x, r.2.max().y],
                        ),
                        i,
                    )
                })
                .collect(),
        );
        let mut index: Vec<Option<RingIndex>> = (0..n).map(|_| None).collect();
        for i in 1..n {
            let b = &rings[i].2;
            let env = rstar::AABB::from_corners([b.min().x, b.min().y], [b.max().x, b.max().y]);
            // Candidates: bigger rings whose box contains this ring's box;
            // the smallest one that contains the ring is its parent.
            let mut candidates: Vec<usize> = tree
                .locate_in_envelope_intersecting(&env)
                .map(|g| g.data)
                .filter(|&j| j < i)
                .filter(|&j| {
                    let o = &rings[j].2;
                    o.min().x <= b.min().x && o.max().x >= b.max().x && o.min().y <= b.min().y && o.max().y >= b.max().y
                })
                .collect();
            candidates.sort_unstable_by(|a, b| b.cmp(a));
            for j in candidates {
                let idx = index[j].get_or_insert_with(|| RingIndex::new(&rings[j].0 .0));
                if idx.votes(&rings[i].0 .0) > 0 {
                    parent[i] = Some(j);
                    depth[i] = depth[j] + 1;
                    break;
                }
            }
        }
    }
    let mut polys: Vec<Polygon> = Vec::new();
    let mut poly_of: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        if depth[i].is_multiple_of(2) {
            poly_of[i] = Some(polys.len());
            polys.push(Polygon::new(rings[i].0.clone(), vec![]));
        } else if let Some(p) = parent[i].and_then(|p| poly_of[p]) {
            let hole = rings[i].0.clone();
            polys[p].interiors_push(hole);
        }
    }
    let mut multipolygon = MultiPolygon::new(polys);
    orient_multipolygon(&mut multipolygon);
    Assembled {
        multipolygon,
        open_rings,
        crossing: false,
    }
}

/// Whether any two segments intersect anywhere except at an endpoint they
/// share (exact integer arithmetic). A sweep along x keeps only the
/// segments spanning the current x active, which for polygon outlines is a
/// handful, so this is about O(n log n).
pub fn has_crossings(segments: &[(LatpLon, LatpLon)]) -> bool {
    #[derive(Clone, Copy)]
    struct S {
        a: (i64, i64),
        b: (i64, i64),
        min_x: i64,
        max_x: i64,
        min_y: i64,
        max_y: i64,
    }
    let mut segs: Vec<S> = segments
        .iter()
        .filter(|(a, b)| a != b)
        .map(|&(a, b)| {
            let a = (a.lon as i64, a.latp as i64);
            let b = (b.lon as i64, b.latp as i64);
            S {
                a,
                b,
                min_x: a.0.min(b.0),
                max_x: a.0.max(b.0),
                min_y: a.1.min(b.1),
                max_y: a.1.max(b.1),
            }
        })
        .collect();
    segs.sort_unstable_by_key(|s| s.min_x);
    let orient = |p: (i64, i64), q: (i64, i64), r: (i64, i64)| -> i8 {
        let v = (q.0 - p.0) as i128 * (r.1 - p.1) as i128 - (q.1 - p.1) as i128 * (r.0 - p.0) as i128;
        v.signum() as i8
    };
    let on_segment = |p: (i64, i64), s: &S| p.0 >= s.min_x && p.0 <= s.max_x && p.1 >= s.min_y && p.1 <= s.max_y;
    let shares_endpoint = |s: &S, t: &S| s.a == t.a || s.a == t.b || s.b == t.a || s.b == t.b;
    let crosses = |s: &S, t: &S| -> bool {
        if s.max_y < t.min_y || t.max_y < s.min_y {
            return false;
        }
        let (o1, o2, o3, o4) = (
            orient(s.a, s.b, t.a),
            orient(s.a, s.b, t.b),
            orient(t.a, t.b, s.a),
            orient(t.a, t.b, s.b),
        );
        if shares_endpoint(s, t) {
            // Touching at a shared node is fine; overlapping along a line is not.
            if o1 == 0 && o2 == 0 {
                let other_t = if t.a == s.a || t.a == s.b { t.b } else { t.a };
                let other_s = if s.a == t.a || s.a == t.b { s.b } else { s.a };
                return (other_t != s.a && other_t != s.b && on_segment(other_t, s))
                    || (other_s != t.a && other_s != t.b && on_segment(other_s, t));
            }
            return false;
        }
        if o1 != o2 && o3 != o4 && o1 != 0 && o2 != 0 && o3 != 0 && o4 != 0 {
            return true;
        }
        (o1 == 0 && on_segment(t.a, s))
            || (o2 == 0 && on_segment(t.b, s))
            || (o3 == 0 && on_segment(s.a, t))
            || (o4 == 0 && on_segment(s.b, t))
    };
    let mut active: Vec<S> = Vec::new();
    for s in segs {
        active.retain(|t| t.max_x >= s.min_x);
        if active.iter().any(|t| crosses(&s, t)) {
            return true;
        }
        active.push(s);
    }
    false
}

/// Point-in-ring tests against a ring's edges binned into horizontal
/// bands, so a test costs about the square root of the ring's size instead
/// of all of it (country outlines have millions of points).
pub struct RingIndex {
    edges: Vec<(crate::Coord, crate::Coord)>,
    min_y: f64,
    max_y: f64,
    band_h: f64,
    /// Per band, the edges overlapping it (indices into `edges`).
    bands: Vec<Vec<u32>>,
}

impl RingIndex {
    /// Index a ring (closed or not).
    pub fn new(ring: &[crate::Coord]) -> Self {
        let n = ring.len();
        let mut edges = Vec::with_capacity(n);
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            if a != b {
                edges.push((a, b));
            }
        }
        let (mut min_y, mut max_y) = (f64::INFINITY, f64::NEG_INFINITY);
        for c in ring {
            min_y = min_y.min(c.y);
            max_y = max_y.max(c.y);
        }
        let nb = if edges.len() < 64 {
            1
        } else {
            ((edges.len() as f64).sqrt() as usize).max(1)
        };
        let band_h = ((max_y - min_y) / nb as f64).max(f64::MIN_POSITIVE);
        let mut bands = vec![Vec::new(); nb];
        let band_of = |y: f64| (((y - min_y) / band_h) as usize).min(nb - 1);
        for (k, (a, b)) in edges.iter().enumerate() {
            let (lo, hi) = (band_of(a.y.min(b.y)), band_of(a.y.max(b.y)));
            for band in &mut bands[lo..=hi] {
                band.push(k as u32);
            }
        }
        Self {
            edges,
            min_y,
            max_y,
            band_h,
            bands,
        }
    }

    /// Even–odd test; `None` when the point is on the ring.
    pub fn contains(&self, p: crate::Coord) -> Option<bool> {
        if self.bands.is_empty() || p.y < self.min_y || p.y > self.max_y {
            return Some(false);
        }
        let band = (((p.y - self.min_y) / self.band_h) as usize).min(self.bands.len() - 1);
        let list = &self.bands[band];
        let mut inside = false;
        for &k in list {
            let (a, b) = self.edges[k as usize];
            if a == p || b == p {
                return None;
            }
            if (a.y > p.y) != (b.y > p.y) {
                let x = (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x;
                if x == p.x {
                    return None;
                }
                if p.x < x {
                    inside = !inside;
                }
            } else if a.y == p.y && b.y == p.y && p.x >= a.x.min(b.x) && p.x <= a.x.max(b.x) {
                return None;
            }
        }
        Some(inside)
    }

    /// Positive when most sampled vertices of `pts` not on the ring are inside.
    pub fn votes(&self, pts: &[crate::Coord]) -> i32 {
        let mut votes = 0;
        let mut counted = 0;
        let step = (pts.len() / 7).max(1);
        for c in pts.iter().step_by(step) {
            match self.contains(*c) {
                Some(true) => votes += 1,
                Some(false) => votes -= 1,
                None => continue,
            }
            counted += 1;
            if counted >= 7 {
                break;
            }
        }
        votes
    }
}

/// Steps 1 and 2 of [`assemble_area`]: simple closed rings (first point
/// repeated last) and the number of open runs left over.
pub fn build_rings(ways: &[&[LatpLon]]) -> (Vec<Vec<LatpLon>>, usize) {
    // 1. Segments, normalised, with duplicates cancelled in pairs.
    let mut segs: Vec<(LatpLon, LatpLon)> = Vec::new();
    for w in ways {
        for p in w.windows(2) {
            if p[0] != p[1] {
                segs.push(if p[0] < p[1] { (p[0], p[1]) } else { (p[1], p[0]) });
            }
        }
    }
    segs.sort_unstable();
    let mut kept: Vec<(LatpLon, LatpLon)> = Vec::with_capacity(segs.len());
    let mut i = 0;
    while i < segs.len() {
        let mut j = i + 1;
        while j < segs.len() && segs[j] == segs[i] {
            j += 1;
        }
        if (j - i) % 2 == 1 {
            kept.push(segs[i]);
        }
        i = j;
    }

    // 2. Walk. Locations become dense indices and adjacency a CSR array,
    // so the walk is plain indexing (country outlines have millions of
    // segments).
    // Sort the endpoints once and number distinct locations in order
    // (cheaper than a binary search per endpoint).
    let mut eps: Vec<(LatpLon, u32)> = Vec::with_capacity(kept.len() * 2);
    for (k, &(a, b)) in kept.iter().enumerate() {
        eps.push((a, (k as u32) << 1));
        eps.push((b, ((k as u32) << 1) | 1));
    }
    eps.sort_unstable_by_key(|e| e.0);
    let mut nodes: Vec<LatpLon> = Vec::with_capacity(eps.len() / 2 + 1);
    let mut ends: Vec<(u32, u32)> = vec![(0, 0); kept.len()];
    for &(loc, tag) in &eps {
        if nodes.last() != Some(&loc) {
            nodes.push(loc);
        }
        let id = nodes.len() as u32 - 1;
        let e = &mut ends[(tag >> 1) as usize];
        if tag & 1 == 0 {
            e.0 = id;
        } else {
            e.1 = id;
        }
    }
    drop(eps);
    let mut offsets = vec![0u32; nodes.len() + 1];
    for &(a, b) in &ends {
        offsets[a as usize + 1] += 1;
        offsets[b as usize + 1] += 1;
    }
    for i in 0..nodes.len() {
        offsets[i + 1] += offsets[i];
    }
    let mut fill = offsets.clone();
    let mut adj = vec![0u32; ends.len() * 2];
    for (k, &(a, b)) in ends.iter().enumerate() {
        adj[fill[a as usize] as usize] = k as u32;
        fill[a as usize] += 1;
        adj[fill[b as usize] as usize] = k as u32;
        fill[b as usize] += 1;
    }
    drop(fill);
    // Per node: the first adjacency slot that may still be unused.
    let mut cursor: Vec<u32> = offsets[..nodes.len()].to_vec();
    let mut used = vec![false; ends.len()];
    const NOT_ON_PATH: u32 = u32::MAX;
    let mut pos = vec![NOT_ON_PATH; nodes.len()];
    let next_unused = |node: u32, used: &[bool], cursor: &mut [u32]| -> Option<u32> {
        let end = offsets[node as usize + 1];
        let c = &mut cursor[node as usize];
        while *c < end {
            let k = adj[*c as usize];
            if !used[k as usize] {
                return Some(k);
            }
            *c += 1;
        }
        None
    };
    let mut rings = Vec::new();
    let mut open = 0usize;
    let mut path: Vec<u32> = Vec::new();
    for start in 0..ends.len() {
        if used[start] {
            continue;
        }
        for &n in &path {
            pos[n as usize] = NOT_ON_PATH;
        }
        path.clear();
        let first = ends[start].0;
        path.push(first);
        pos[first as usize] = 0;
        loop {
            let cur = *path.last().unwrap();
            let Some(k) = next_unused(cur, &used, &mut cursor) else {
                if path.len() > 1 {
                    open += 1;
                }
                break;
            };
            used[k as usize] = true;
            let (a, b) = ends[k as usize];
            let next = if a == cur { b } else { a };
            let at = pos[next as usize];
            if at != NOT_ON_PATH {
                let at = at as usize;
                let mut ring: Vec<LatpLon> = path[at..].iter().map(|&n| nodes[n as usize]).collect();
                ring.push(nodes[next as usize]);
                if ring.len() >= 4 {
                    rings.push(ring);
                }
                for &n in &path[at + 1..] {
                    pos[n as usize] = NOT_ON_PATH;
                }
                path.truncate(at + 1);
                if path.len() == 1 && next_unused(path[0], &used, &mut cursor).is_none() {
                    break;
                }
            } else {
                pos[next as usize] = path.len() as u32;
                path.push(next);
            }
        }
    }
    (rings, open)
}

/// Shoelace area of a ring (positive when counter-clockwise).
pub fn signed_ring_area(ring: &[crate::Coord]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for w in ring.windows(2) {
        sum += w[0].x * w[1].y - w[1].x * w[0].y;
    }
    let (f, l) = (ring[0], ring[ring.len() - 1]);
    if f != l {
        sum += l.x * f.y - f.x * l.y;
    }
    sum / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::Area;

    fn ll(x: i32, y: i32) -> LatpLon {
        LatpLon {
            lon: x * 10_000_000,
            latp: y * 10_000_000,
        }
    }

    fn square(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<LatpLon> {
        vec![ll(x0, y0), ll(x1, y0), ll(x1, y1), ll(x0, y1), ll(x0, y0)]
    }

    #[test]
    fn merges_reversed_and_split_ways() {
        let a = vec![ll(0, 0), ll(10, 0), ll(10, 10)];
        let b = vec![ll(0, 0), ll(0, 10)]; // reversed direction
        let c = vec![ll(10, 10), ll(0, 10)];
        let merged = merge_ways(&[&a, &c, &b]);
        assert_eq!(merged.len(), 1);
        let r = &merged[0];
        assert_eq!(r.first(), r.last());
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn roles_assign_holes() {
        let outer = square(0, 0, 10, 10);
        let inner = square(2, 2, 4, 4);
        let mp = multipolygon_from_roles(&[&outer], &[&inner]);
        assert_eq!(mp.0.len(), 1);
        assert_eq!(mp.0[0].interiors().len(), 1);
        assert!((mp.unsigned_area() - 96.0).abs() < 1e-9);
        assert!(mp.signed_area() > 0.0);
    }

    #[test]
    fn containment_nesting_ignores_roles() {
        let outer = square(0, 0, 10, 10);
        let hole = square(2, 2, 8, 8);
        let island = square(4, 4, 6, 6);
        let other = square(20, 20, 30, 30);
        let a = assemble_area(&[&island, &other, &hole, &outer]);
        assert_eq!(a.open_rings, 0);
        assert_eq!(a.multipolygon.0.len(), 3);
        let area = a.multipolygon.unsigned_area();
        assert!((area - (100.0 - 36.0 + 4.0 + 100.0)).abs() < 1e-9, "{area}");
    }

    #[test]
    fn touching_hole_still_nests() {
        let outer = square(0, 0, 10, 10);
        // A hole that shares the outer's corner.
        let hole = vec![ll(0, 0), ll(5, 2), ll(2, 5), ll(0, 0)];
        let a = assemble_area(&[&outer, &hole]);
        assert_eq!(a.multipolygon.0.len(), 1);
        assert_eq!(a.multipolygon.0[0].interiors().len(), 1);
    }

    #[test]
    fn shared_edges_cancel() {
        // Two adjacent squares as separate closed ways: one 2×1 ring.
        let a = square(0, 0, 1, 1);
        let b = square(1, 0, 2, 1);
        let (rings, open) = build_rings(&[&a, &b]);
        assert_eq!(open, 0);
        assert_eq!(rings.len(), 1);
        assert_eq!(rings[0].len(), 7);
        let a = assemble_area(&[&a, &b]);
        assert!((a.multipolygon.unsigned_area() - 2.0).abs() < 1e-9);
    }

    #[test]
    fn touching_rings_split() {
        // A figure of eight drawn as one way: two rings.
        let eight = vec![
            ll(0, 0),
            ll(1, 0),
            ll(1, 1),
            ll(2, 1),
            ll(2, 2),
            ll(1, 2),
            ll(1, 1),
            ll(0, 1),
            ll(0, 0),
        ];
        let (rings, open) = build_rings(&[&eight]);
        assert_eq!(open, 0);
        assert_eq!(rings.len(), 2);
        let a = assemble_area(&[&eight]);
        assert_eq!(a.multipolygon.0.len(), 2);
    }

    #[test]
    fn ring_index_agrees_with_plain_test() {
        // A star-ish ring with many vertices.
        let n = 2000;
        let ring: Vec<crate::Coord> = (0..=n)
            .map(|i| {
                let t = (i % n) as f64 / n as f64 * std::f64::consts::TAU;
                let r = if i % 2 == 0 { 10.0 } else { 6.0 };
                crate::Coord {
                    x: r * t.cos(),
                    y: r * t.sin(),
                }
            })
            .collect();
        let idx = RingIndex::new(&ring);
        let mut s = 12345u64;
        for _ in 0..5000 {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let x = ((s >> 11) as f64 / (1u64 << 53) as f64) * 24.0 - 12.0;
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let y = ((s >> 11) as f64 / (1u64 << 53) as f64) * 24.0 - 12.0;
            let p = crate::Coord { x, y };
            assert_eq!(idx.contains(p), point_in_ring(p, &ring), "{p:?}");
        }
        for c in &ring {
            assert_eq!(idx.contains(*c), None);
        }
    }

    #[test]
    fn many_islands_nest_quickly() {
        // One big square with 10,000 small islands inside and one lake.
        let mut ways: Vec<Vec<LatpLon>> = vec![square(-1, -1, 201, 201)];
        for i in 0..100 {
            for j in 0..100 {
                ways.push(square(i * 2, j * 2, i * 2 + 1, j * 2 + 1));
            }
        }
        let refs: Vec<&[LatpLon]> = ways.iter().map(|w| w.as_slice()).collect();
        let started = std::time::Instant::now();
        let a = assemble_area(&refs);
        assert!(started.elapsed().as_secs_f64() < 5.0);
        // Every small square is a hole in the big one.
        assert_eq!(a.multipolygon.0.len(), 1);
        assert_eq!(a.multipolygon.0[0].interiors().len(), 10_000);
    }

    #[test]
    fn crossings() {
        let segs = |pts: &[(i32, i32, i32, i32)]| -> Vec<(LatpLon, LatpLon)> {
            pts.iter().map(|&(a, b, c, d)| (ll(a, b), ll(c, d))).collect()
        };
        // A bow-tie crosses itself.
        assert!(has_crossings(&segs(&[
            (0, 0, 2, 2),
            (2, 2, 2, 0),
            (2, 0, 0, 2),
            (0, 2, 0, 0)
        ])));
        // A square does not.
        assert!(!has_crossings(&segs(&[
            (0, 0, 2, 0),
            (2, 0, 2, 2),
            (2, 2, 0, 2),
            (0, 2, 0, 0)
        ])));
        // Two squares touching at a corner do not.
        assert!(!has_crossings(&segs(&[
            (0, 0, 1, 0),
            (1, 0, 1, 1),
            (1, 1, 0, 1),
            (0, 1, 0, 0),
            (1, 1, 2, 1),
            (2, 1, 2, 2),
            (2, 2, 1, 2),
            (1, 2, 1, 1)
        ])));
        // A T-junction (an endpoint inside another segment) does.
        assert!(has_crossings(&segs(&[(0, 0, 4, 0), (2, 0, 2, 2)])));
        // Collinear overlap does.
        assert!(has_crossings(&segs(&[(0, 0, 4, 0), (2, 0, 6, 0)])));
        assert!(has_crossings(&segs(&[(0, 0, 4, 0), (0, 0, 2, 0)])));
        // Collinear segments meeting end to end do not.
        assert!(!has_crossings(&segs(&[(0, 0, 2, 0), (2, 0, 4, 0)])));
        let a = assemble_area(&[&[ll(0, 0), ll(2, 2), ll(2, 0), ll(0, 2), ll(0, 0)][..]]);
        assert!(a.crossing && a.multipolygon.0.is_empty());
    }

    #[test]
    fn open_rings_are_reported() {
        let line = vec![ll(0, 0), ll(5, 5)];
        let a = assemble_area(&[&line]);
        assert_eq!(a.open_rings, 1);
        assert!(a.multipolygon.0.is_empty());
    }

    #[test]
    fn point_in_ring_boundary() {
        let r: Vec<crate::Coord> = square(0, 0, 10, 10).iter().map(|&l| ll_to_coord(l)).collect();
        assert_eq!(point_in_ring(crate::Coord { x: 5.0, y: 5.0 }, &r), Some(true));
        assert_eq!(point_in_ring(crate::Coord { x: 15.0, y: 5.0 }, &r), Some(false));
        assert_eq!(point_in_ring(crate::Coord { x: 0.0, y: 5.0 }, &r), None);
    }
}
