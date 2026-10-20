//! Point-in-polygon lookups (`PolygonIndex` in TypeScript): polygons are
//! added from JS, then the index is built and queried, one point at a time
//! or many on worker threads.

use std::sync::Arc;

use napi::bindgen_prelude::{AsyncTask, Float64Array, Int32Array, Uint32Array};
use napi::{Env, Result, Task};
use napi_derive::napi;
use tilemaker_export::PolygonIndex;

use crate::options::invalid;

enum State {
    Adding(Box<PolygonIndex>),
    Built(Arc<PolygonIndex>),
}

/// A polygon index being filled, then built and queried.
#[napi]
pub struct NativePolygonIndex {
    state: State,
}

impl NativePolygonIndex {
    fn built(&self) -> Result<&Arc<PolygonIndex>> {
        match &self.state {
            State::Built(i) => Ok(i),
            State::Adding(_) => Err(invalid("the index is not built yet")),
        }
    }
}

#[napi]
impl NativePolygonIndex {
    /// An empty index.
    #[napi(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            state: State::Adding(Box::default()),
        }
    }

    /// Add a multipolygon (flat coordinates, ring ends, polygon ends);
    /// its index.
    #[napi]
    pub fn add(&mut self, coords: Float64Array, parts: Uint32Array, polygons: Uint32Array) -> Result<u32> {
        match &mut self.state {
            State::Adding(i) => {
                let (np, nr) = (coords.len() / 2, parts.len());
                if parts.iter().any(|&e| e as usize > np) || polygons.iter().any(|&e| e as usize > nr) {
                    return Err(invalid("parts or polygons point past the coordinates"));
                }
                Ok(i.add(&coords, &parts, &polygons))
            }
            State::Built(_) => Err(invalid("the index is already built")),
        }
    }

    /// Build the index (no more adding).
    #[napi]
    pub fn build(&mut self) {
        if let State::Adding(i) = &mut self.state {
            let mut i = std::mem::take(i);
            i.build();
            self.state = State::Built(Arc::from(i));
        }
    }

    /// Number of polygons added.
    #[napi(getter)]
    pub fn size(&self) -> u32 {
        match &self.state {
            State::Adding(i) => i.len() as u32,
            State::Built(i) => i.len() as u32,
        }
    }

    /// Polygon `i`'s area in m².
    #[napi]
    pub fn area(&self, i: u32) -> Result<f64> {
        let idx = self.built()?;
        if (i as usize) < idx.len() {
            Ok(idx.area(i))
        } else {
            Err(invalid("no such polygon"))
        }
    }

    /// The polygons containing a point, smallest first.
    #[napi]
    pub fn lookup(&self, lon: f64, lat: f64) -> Result<Uint32Array> {
        Ok(Uint32Array::new(self.built()?.lookup(lon, lat)))
    }

    /// The smallest polygon containing a point, or -1.
    #[napi]
    pub fn smallest(&self, lon: f64, lat: f64) -> Result<i32> {
        Ok(self.built()?.smallest(lon, lat).map_or(-1, |i| i as i32))
    }

    /// For each `lon, lat` pair, the smallest polygon containing it (or
    /// -1), computed on worker threads.
    #[napi(ts_return_type = "Promise<Int32Array>")]
    pub fn smallest_many(&self, points: Float64Array, threads: Option<u32>) -> Result<AsyncTask<SmallestMany>> {
        Ok(AsyncTask::new(SmallestMany {
            index: self.built()?.clone(),
            points: points.to_vec(),
            threads: threads.unwrap_or(0) as usize,
        }))
    }
}

/// Lookups off the JS thread.
pub struct SmallestMany {
    index: Arc<PolygonIndex>,
    points: Vec<f64>,
    threads: usize,
}

impl Task for SmallestMany {
    type Output = Vec<i32>;
    type JsValue = Int32Array;

    fn compute(&mut self) -> Result<Vec<i32>> {
        Ok(self
            .index
            .smallest_many(&self.points, self.threads)
            .into_iter()
            .map(|i| i.map_or(-1, |i| i as i32))
            .collect())
    }

    fn resolve(&mut self, _env: Env, out: Vec<i32>) -> Result<Int32Array> {
        Ok(Int32Array::new(out))
    }
}
