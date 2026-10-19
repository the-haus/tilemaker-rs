//! Which tiles a geometry touches (port of `insertIntermediateTiles` and
//! `fillCoveredTiles`).

use std::collections::HashSet;

use crate::coords::*;
use crate::Coord;

/// Add the base-zoom tiles along the polyline `points` to `set`: each
/// segment's end tiles and the supercover between them (Dedu's
/// Bresenham-based algorithm, as tilemaker uses, so tile sets match).
pub fn insert_intermediate_tiles(points: &[Coord], z: u8, set: &mut HashSet<TileCoord>) {
    let max = ((1u64 << z) - 1) as f64;
    let tile = |c: Coord| -> (i64, i64) {
        let y = c.y.clamp(MIN_LATP, MAX_LATP);
        (
            lon2tilexf(c.x, z).clamp(0.0, max) as i64,
            latp2tileyf(y, z).clamp(0.0, max) as i64,
        )
    };
    let mut insert = |x: i64, y: i64| {
        if x >= 0 && y >= 0 {
            set.insert(TileCoord::new(x as u32, y as u32));
        }
    };
    let mut prev: Option<(i64, i64)> = None;
    for &c in points {
        let (x2, y2) = tile(c);
        insert(x2, y2);
        let Some((x1, y1)) = prev.replace((x2, y2)) else {
            continue;
        };
        let (mut x, mut y) = (x1, y1);
        let (mut dx, mut dy) = (x2 - x1, y2 - y1);
        let ystep = if dy < 0 {
            dy = -dy;
            -1
        } else {
            1
        };
        let xstep = if dx < 0 {
            dx = -dx;
            -1
        } else {
            1
        };
        let (ddy, ddx) = (2 * dy, 2 * dx);
        if ddx >= ddy {
            let mut error = dx;
            let mut errorprev = dx;
            for _ in 0..dx {
                x += xstep;
                error += ddy;
                if error > ddx {
                    y += ystep;
                    error -= ddx;
                    if error + errorprev < ddx {
                        insert(x, y - ystep);
                    } else if error + errorprev > ddx {
                        insert(x - xstep, y);
                    } else {
                        insert(x, y - ystep);
                        insert(x - xstep, y);
                    }
                }
                insert(x, y);
                errorprev = error;
            }
        } else {
            let mut error = dy;
            let mut errorprev = dy;
            for _ in 0..dy {
                y += ystep;
                error += ddx;
                if error > ddy {
                    x += xstep;
                    error -= ddy;
                    if error + errorprev < ddy {
                        insert(x - xstep, y);
                    } else if error + errorprev > ddy {
                        insert(x, y - ystep);
                    } else {
                        insert(x - xstep, y);
                        insert(x, y - ystep);
                    }
                }
                insert(x, y);
                errorprev = error;
            }
        }
    }
}

/// Fill the gaps between each column's smallest and largest row, so a
/// polygon's outline becomes its full coverage.
pub fn fill_covered_tiles(set: &mut HashSet<TileCoord>) {
    let mut list: Vec<TileCoord> = set.iter().copied().collect();
    list.sort_unstable();
    let mut i = 0;
    while i < list.len() {
        let x = list[i].x;
        let mut j = i;
        while j + 1 < list.len() && list[j + 1].x == x {
            j += 1;
        }
        let (lo, hi) = (list[i].y, list[j].y);
        for y in lo..=hi {
            set.insert(TileCoord::new(x, y));
        }
        i = j + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(lon: f64, lat: f64) -> Coord {
        Coord {
            x: lon,
            y: lat2latp(lat),
        }
    }

    #[test]
    fn diagonal_line_is_connected() {
        let mut set = HashSet::new();
        insert_intermediate_tiles(&[c(-170.0, 80.0), c(170.0, -80.0)], 3, &mut set);
        assert!(set.contains(&TileCoord::new(0, 0)));
        assert!(set.contains(&TileCoord::new(7, 7)));
        // Every step moves one tile horizontally or vertically, so the walk
        // touches at least 15 tiles (7 + 7 + 1).
        assert!(set.len() >= 15, "{}", set.len());
    }

    #[test]
    fn fills_columns() {
        let mut set: HashSet<_> = [TileCoord::new(1, 1), TileCoord::new(1, 5), TileCoord::new(2, 3)]
            .into_iter()
            .collect();
        fill_covered_tiles(&mut set);
        assert_eq!(set.len(), 6);
        assert!(set.contains(&TileCoord::new(1, 3)));
    }
}
