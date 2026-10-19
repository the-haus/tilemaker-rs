//! tilemaker's `config.json`: settings and layers (port of `shared_data.cpp`).
//!
//! ```json
//! {
//!   "layers": { "water": { "minzoom": 6, "maxzoom": 14, "simplify_below": 12 } },
//!   "settings": { "minzoom": 0, "maxzoom": 14, "basezoom": 14, "include_ids": false,
//!                 "name": "…", "version": "3.0", "description": "…", "compress": "gzip" }
//! }
//! ```

use std::collections::BTreeMap;

use serde_json::Value;
use tilemaker_geom::simplify::SimplifyAlgorithm;

/// A config error.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Not JSON.
    #[error("config is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Reading the file failed.
    #[error("cannot read config: {0}")]
    Io(#[from] std::io::Error),
    /// A missing or wrongly typed field.
    #[error("config: {0}")]
    Invalid(String),
}

fn invalid(msg: impl Into<String>) -> ConfigError {
    ConfigError::Invalid(msg.into())
}

/// Tile compression.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compress {
    /// gzip.
    Gzip,
    /// zlib ("deflate").
    Deflate,
    /// None.
    None,
}

/// One layer (see tilemaker's CONFIGURATION.md).
#[derive(Clone, Debug, PartialEq)]
pub struct LayerDef {
    /// Name.
    pub name: String,
    /// Lowest zoom written.
    pub minzoom: u8,
    /// Highest zoom written.
    pub maxzoom: u8,
    /// Simplify below this zoom.
    pub simplify_below: u8,
    /// Simplification distance at `simplify_below - 1`, in degrees.
    pub simplify_level: f64,
    /// Simplification distance in metres (overrides `simplify_level`).
    pub simplify_length: f64,
    /// Factor applied per zoom further down.
    pub simplify_ratio: f64,
    /// Algorithm.
    pub simplify_algorithm: SimplifyAlgorithm,
    /// Drop small polygons below this zoom.
    pub filter_below: u8,
    /// Area threshold at `filter_below - 1`, in square metres-ish (as tilemaker).
    pub filter_area: f64,
    /// Sort features by ascending `z_order`.
    pub z_order_ascending: bool,
    /// Keep at most this many features per tile (0: no limit).
    pub feature_limit: u32,
    /// Apply `feature_limit` below this zoom.
    pub feature_limit_below: u8,
    /// Merge points with identical attributes into multipoints.
    pub combine_points: bool,
    /// Merge lines with identical attributes below this zoom.
    pub combine_lines_below: u8,
    /// Union polygons with identical attributes below this zoom.
    pub combine_polygons_below: u8,
    /// External source (shapefile/GeoJSON): not supported, kept for warnings.
    pub source: Option<String>,
    /// Write into this other layer.
    pub write_to: Option<String>,
    /// Thin out points below this zoom (declutter; accepted, not applied).
    pub declutter_below: Option<u8>,
}

/// Layers, in output order, with `write_to` layers grouped under their
/// target (port of `LayerDefinition`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerDefinition {
    /// All layers, in config order.
    pub layers: Vec<LayerDef>,
    /// Name → index.
    pub layer_map: BTreeMap<String, usize>,
    /// Output layers: each is its own index followed by the indices that
    /// write into it.
    pub layer_order: Vec<Vec<usize>>,
}

impl LayerDefinition {
    fn add(&mut self, layer: LayerDef) -> Result<usize, ConfigError> {
        let n = self.layers.len();
        if n >= 255 {
            return Err(invalid("at most 255 layers are supported"));
        }
        if self.layer_map.contains_key(&layer.name) {
            return Err(invalid(format!("layer {:?} is defined twice", layer.name)));
        }
        match &layer.write_to {
            None => self.layer_order.push(vec![n]),
            Some(target) => {
                let t = *self.layer_map.get(target).ok_or_else(|| {
                    invalid(format!(
                        "layer {:?}: write_to {target:?} doesn't exist (it must come first)",
                        layer.name
                    ))
                })?;
                for group in &mut self.layer_order {
                    if group[0] == t {
                        group.push(n);
                    }
                }
            }
        }
        self.layer_map.insert(layer.name.clone(), n);
        self.layers.push(layer);
        Ok(n)
    }

    /// Index of a layer.
    pub fn index(&self, name: &str) -> Option<usize> {
        self.layer_map.get(name).copied()
    }
}

/// The whole config.
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    /// Lowest zoom generated.
    pub minzoom: u8,
    /// Highest zoom generated.
    pub maxzoom: u8,
    /// Zoom of the tile index (must be ≥ maxzoom).
    pub basezoom: u8,
    /// Write OSM ids as feature ids.
    pub include_ids: bool,
    /// 8192-unit tiles at maxzoom.
    pub high_resolution: bool,
    /// Tile compression.
    pub compress: Compress,
    /// Default for each layer's `combine_lines_below`.
    pub combine_below: u8,
    /// MVT version written in layers.
    pub mvt_version: u32,
    /// Tileset name.
    pub name: String,
    /// Tileset version.
    pub version: String,
    /// Tileset description.
    pub description: String,
    /// `[west, south, east, north]` to restrict output to.
    pub bounding_box: Option<[f64; 4]>,
    /// Default view `(lon, lat, zoom)`.
    pub default_view: Option<(f64, f64, u8)>,
    /// `settings.filemetadata` entries.
    pub file_metadata: Vec<(String, Value)>,
    /// Layers.
    pub layers: LayerDefinition,
}

fn get_u8(v: &Value, key: &str, default: Option<u8>) -> Result<u8, ConfigError> {
    match v.get(key) {
        None | Some(Value::Null) => default.ok_or_else(|| invalid(format!("missing {key:?}"))),
        Some(x) => x
            .as_i64()
            .filter(|n| (0..=255).contains(n))
            .map(|n| n as u8)
            .ok_or_else(|| invalid(format!("{key:?} must be an integer from 0 to 255"))),
    }
}

fn get_f64(v: &Value, key: &str, default: f64) -> Result<f64, ConfigError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(x) => x.as_f64().ok_or_else(|| invalid(format!("{key:?} must be a number"))),
    }
}

fn get_bool(v: &Value, key: &str, default: bool) -> Result<bool, ConfigError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(x) => x
            .as_bool()
            .ok_or_else(|| invalid(format!("{key:?} must be true or false"))),
    }
}

fn get_str(v: &Value, key: &str) -> Result<Option<String>, ConfigError> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(invalid(format!("{key:?} must be a string"))),
    }
}

impl Config {
    /// Parse from JSON text.
    pub fn from_json(text: &str) -> Result<Self, ConfigError> {
        Self::from_value(&serde_json::from_str(text)?)
    }

    /// Read a file.
    pub fn from_path(path: impl AsRef<std::path::Path>) -> Result<Self, ConfigError> {
        Self::from_json(&std::fs::read_to_string(path)?)
    }

    /// Parse from a JSON value.
    pub fn from_value(json: &Value) -> Result<Self, ConfigError> {
        let s = json.get("settings").ok_or_else(|| invalid("missing \"settings\""))?;
        let minzoom = get_u8(s, "minzoom", None)?;
        let maxzoom = get_u8(s, "maxzoom", None)?;
        let basezoom = get_u8(s, "basezoom", Some(maxzoom))?;
        if maxzoom > basezoom {
            return Err(invalid("maxzoom must be the same or smaller than basezoom"));
        }
        if minzoom > maxzoom {
            return Err(invalid("minzoom must not be greater than maxzoom"));
        }
        if basezoom > 22 {
            return Err(invalid("basezoom above 22 is not supported"));
        }
        let compress = match get_str(s, "compress")?.as_deref().unwrap_or("gzip") {
            "gzip" => Compress::Gzip,
            "deflate" => Compress::Deflate,
            "none" => Compress::None,
            other => {
                return Err(invalid(format!(
                    "\"compress\" should be \"gzip\", \"deflate\" or \"none\", not {other:?}"
                )))
            }
        };
        let combine_below = get_u8(s, "combine_below", Some(0))?;
        let bounding_box = match s.get("bounding_box") {
            None | Some(Value::Null) => None,
            Some(Value::Array(a)) if a.len() == 4 && a.iter().all(Value::is_number) => {
                let v: Vec<f64> = a.iter().map(|x| x.as_f64().unwrap()).collect();
                Some([v[0], v[1], v[2], v[3]])
            }
            Some(_) => return Err(invalid("\"bounding_box\" must be [minlon, minlat, maxlon, maxlat]")),
        };
        let default_view = match s.get("default_view") {
            Some(Value::Array(a)) if a.len() == 3 => Some((
                a[0].as_f64().unwrap_or(0.0),
                a[1].as_f64().unwrap_or(0.0),
                a[2].as_u64().unwrap_or(0) as u8,
            )),
            _ => None,
        };
        let file_metadata = match s.get("filemetadata") {
            Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            _ => Vec::new(),
        };

        let mut layers = LayerDefinition::default();
        let lj = json
            .get("layers")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("missing \"layers\" object"))?;
        for (name, v) in lj {
            let ctx = |e: ConfigError| invalid(format!("layer {name:?}: {e}"));
            let minz = get_u8(v, "minzoom", None).map_err(ctx)?;
            let maxz = get_u8(v, "maxzoom", None).map_err(ctx)?;
            let feature_limit = get_f64(v, "feature_limit", 0.0).map_err(ctx)?.max(0.0) as u32;
            let algo = match get_str(v, "simplify_algorithm").map_err(ctx)?.as_deref() {
                Some("visvalingam") => SimplifyAlgorithm::Visvalingam,
                Some("buildings") => SimplifyAlgorithm::Buildings,
                _ => SimplifyAlgorithm::DouglasPeucker,
            };
            let declutter_below = match v.get("declutter_below") {
                None | Some(Value::Null) => None,
                Some(_) => Some(get_u8(v, "declutter_below", None).map_err(ctx)?.min(15)),
            };
            layers.add(LayerDef {
                name: name.clone(),
                minzoom: minz,
                maxzoom: maxz,
                simplify_below: get_u8(v, "simplify_below", Some(0)).map_err(ctx)?,
                simplify_level: get_f64(v, "simplify_level", 0.01).map_err(ctx)?,
                simplify_length: get_f64(v, "simplify_length", 0.0).map_err(ctx)?,
                simplify_ratio: get_f64(v, "simplify_ratio", 2.0).map_err(ctx)?,
                simplify_algorithm: algo,
                filter_below: get_u8(v, "filter_below", Some(0)).map_err(ctx)?,
                filter_area: get_f64(v, "filter_area", 0.5).map_err(ctx)?,
                z_order_ascending: get_bool(v, "z_order_ascending", feature_limit == 0).map_err(ctx)?,
                feature_limit,
                feature_limit_below: get_u8(v, "feature_limit_below", Some(maxz.saturating_add(1))).map_err(ctx)?,
                combine_points: get_bool(v, "combine_points", true).map_err(ctx)?,
                combine_lines_below: get_u8(v, "combine_lines_below", Some(combine_below)).map_err(ctx)?,
                combine_polygons_below: get_u8(v, "combine_polygons_below", Some(0)).map_err(ctx)?,
                source: get_str(v, "source").map_err(ctx)?,
                write_to: get_str(v, "write_to").map_err(ctx)?,
                declutter_below,
            })?;
        }

        Ok(Config {
            minzoom,
            maxzoom,
            basezoom,
            include_ids: get_bool(s, "include_ids", false)?,
            high_resolution: get_bool(s, "high_resolution", false)?,
            compress,
            combine_below,
            mvt_version: get_u8(s, "mvt_version", Some(2))? as u32,
            name: get_str(s, "name")?.unwrap_or_default(),
            version: get_str(s, "version")?.unwrap_or_default(),
            description: get_str(s, "description")?.unwrap_or_default(),
            bounding_box,
            default_view,
            file_metadata,
            layers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"{
        "layers": {
            "water": { "minzoom": 6, "maxzoom": 14, "simplify_below": 12, "simplify_level": 0.0003, "simplify_algorithm": "visvalingam" },
            "ocean": { "minzoom": 0, "maxzoom": 14, "write_to": "water", "feature_limit": 10 },
            "poi": { "minzoom": 12, "maxzoom": 14 }
        },
        "settings": {
            "minzoom": 0, "maxzoom": 14, "basezoom": 14, "include_ids": true,
            "name": "Test", "version": "1.0", "description": "d", "compress": "deflate",
            "combine_below": 13, "bounding_box": [1, 2, 3, 4], "default_view": [1.5, 2.5, 7],
            "filemetadata": { "tilejson": "2.0.0", "tiles": ["x"] }
        }
    }"#;

    #[test]
    fn parses_settings_and_layers() {
        let c = Config::from_json(EXAMPLE).unwrap();
        assert_eq!((c.minzoom, c.maxzoom, c.basezoom), (0, 14, 14));
        assert!(c.include_ids);
        assert_eq!(c.compress, Compress::Deflate);
        assert_eq!(c.bounding_box, Some([1.0, 2.0, 3.0, 4.0]));
        assert_eq!(c.default_view, Some((1.5, 2.5, 7)));
        assert_eq!(c.file_metadata.len(), 2);
        let l = &c.layers;
        assert_eq!(l.layers.len(), 3);
        let water = &l.layers[l.index("water").unwrap()];
        assert_eq!(water.simplify_algorithm, SimplifyAlgorithm::Visvalingam);
        assert_eq!(water.combine_lines_below, 13);
        assert!(water.z_order_ascending);
        let ocean = &l.layers[l.index("ocean").unwrap()];
        assert!(!ocean.z_order_ascending, "feature_limit flips the default");
        assert_eq!(ocean.feature_limit_below, 15);
        assert!(l
            .layer_order
            .iter()
            .any(|g| g.len() == 2 && g[0] == l.index("water").unwrap()));
    }

    #[test]
    fn rejects_bad_configs() {
        assert!(Config::from_json("{").is_err());
        assert!(Config::from_json(r#"{"layers":{}}"#).is_err());
        assert!(Config::from_json(r#"{"layers":{},"settings":{"minzoom":0,"maxzoom":15,"basezoom":14}}"#).is_err());
        assert!(Config::from_json(r#"{"layers":{},"settings":{"minzoom":0,"maxzoom":14,"compress":"lzma"}}"#).is_err());
        let e =
            Config::from_json(r#"{"layers":{"a":{"minzoom":"x","maxzoom":1}},"settings":{"minzoom":0,"maxzoom":14}}"#);
        assert!(e.unwrap_err().to_string().contains("layer \"a\""));
        let e = Config::from_json(
            r#"{"layers":{"a":{"minzoom":0,"maxzoom":1,"write_to":"zzz"}},"settings":{"minzoom":0,"maxzoom":14}}"#,
        );
        assert!(e.unwrap_err().to_string().contains("write_to"));
    }
}
