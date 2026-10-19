//! The object being processed and the global functions that read it and
//! write outputs (port of `OsmLuaProcessing`).

use std::collections::HashMap;
use std::sync::Arc;

use mlua::{Lua, MultiValue, Value, Variadic};
use tilemaker_geom::assemble::{multilinestring_from_ways, multipolygon_from_roles};
use tilemaker_geom::compact::{CompactLines, CompactPolygons};
use tilemaker_geom::coords::{latp2lat, LatpLon};
use tilemaker_geom::measure;
use tilemaker_geom::{ll_to_coord, ll_to_linestring, LineString, MultiLineString, MultiPolygon, Point, Polygon};

use crate::output::{Attribute, AttributeValue, GeometryKind, OutputGeometry, PendingOutput, ProcessedObject};
use crate::relations::{MemberKind, ScannedRelations};
use crate::Shared;

/// The type of the object being processed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ObjectKind {
    /// A node.
    #[default]
    Node,
    /// A way.
    Way,
    /// A relation.
    Relation,
}

/// An OSM object handed to the profile.
#[derive(Clone, Debug, Default)]
pub struct Object {
    /// Type.
    pub kind: ObjectKind,
    /// Id.
    pub id: i64,
    /// Tags.
    pub tags: Vec<(String, String)>,
    /// Nodes: the location.
    pub location: Option<LatpLon>,
    /// Ways: node locations.
    pub nodes: Vec<LatpLon>,
    /// Relations: member ways that are not `inner`.
    pub outers: Vec<Vec<LatpLon>>,
    /// Relations: `inner` member ways.
    pub inners: Vec<Vec<LatpLon>>,
    /// Relations: an area (multipolygon/boundary, or inner/outer roles).
    pub closed: bool,
    /// Relations: `type=multipolygon` (processed by `way_function`).
    pub multipolygon: bool,
    /// Relations: node members `(role, location)` for `LayerAsCentroid`.
    pub node_members: Vec<(String, LatpLon)>,
    /// Relations (accepted in the scan) this object belongs to: `(id, role)`.
    pub relations: Vec<(i64, String)>,
}

impl Object {
    fn is_closed(&self) -> bool {
        match self.kind {
            ObjectKind::Node => false,
            ObjectKind::Way => self.nodes.len() > 1 && self.nodes.first() == self.nodes.last(),
            ObjectKind::Relation => self.closed,
        }
    }
}

pub(crate) struct Context {
    pub(crate) shared: Arc<Shared>,
    pub(crate) scanned: Arc<ScannedRelations>,
    pub(crate) postscan: Option<ScannedRelations>,
    pub(crate) accepted: bool,
    obj: Object,
    id: i64,
    relation_cursor: isize,
    outputs: Vec<PendingOutput>,
    geometries: Vec<OutputGeometry>,
    by_kind: HashMap<GeometryKind, Option<usize>>,
    line: Option<Arc<LineString>>,
    polygon: Option<Arc<MultiPolygon>>,
    lines: Option<Arc<MultiLineString>>,
    /// (layer, key) pairs already recorded in `shared.fields`.
    seen_fields: std::collections::HashSet<(u8, String)>,
    /// Warnings already reported by this state.
    seen_warnings: std::collections::HashSet<String>,
}

impl Context {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            scanned: Arc::new(ScannedRelations::new()),
            postscan: None,
            accepted: false,
            obj: Object::default(),
            id: 0,
            relation_cursor: -1,
            outputs: Vec::new(),
            geometries: Vec::new(),
            by_kind: HashMap::new(),
            line: None,
            polygon: None,
            lines: None,
            seen_fields: std::collections::HashSet::new(),
            seen_warnings: std::collections::HashSet::new(),
        }
    }

    fn warn(&mut self, msg: String) {
        if self.seen_warnings.len() < 100 && self.seen_warnings.insert(msg.clone()) {
            self.shared.warn(msg);
        }
    }

    pub(crate) fn begin(&mut self, obj: Object) {
        self.id = obj.id;
        self.obj = obj;
        self.accepted = false;
        self.relation_cursor = -1;
        self.outputs.clear();
        self.geometries.clear();
        self.by_kind.clear();
        self.line = None;
        self.polygon = None;
        self.lines = None;
        if let Some(p) = &self.postscan {
            if self.obj.kind == ObjectKind::Relation {
                self.obj.relations = relations_with_parents(p, self.obj.id);
            }
        }
    }

    pub(crate) fn finish(&mut self) -> ProcessedObject {
        ProcessedObject {
            id: self.id,
            geometries: std::mem::take(&mut self.geometries),
            outputs: std::mem::take(&mut self.outputs),
        }
    }

    fn find(&self, key: &[u8]) -> Option<&str> {
        self.obj
            .tags
            .iter()
            .find(|(k, _)| k.as_bytes() == key)
            .map(|(_, v)| v.as_str())
    }

    fn linestring(&mut self) -> Arc<LineString> {
        if self.line.is_none() {
            self.line = Some(Arc::new(ll_to_linestring(&self.obj.nodes)));
        }
        self.line.clone().unwrap()
    }

    /// The object as an area: a closed way's ring or a relation's
    /// multipolygon, valid and wound counter-clockwise.
    fn multipolygon(&mut self) -> Arc<MultiPolygon> {
        if self.polygon.is_none() {
            let mut mp = match self.obj.kind {
                ObjectKind::Relation => {
                    let outers: Vec<&[LatpLon]> = self.obj.outers.iter().map(|w| w.as_slice()).collect();
                    let inners: Vec<&[LatpLon]> = self.obj.inners.iter().map(|w| w.as_slice()).collect();
                    multipolygon_from_roles(&outers, &inners)
                }
                _ => {
                    let mut ring = ll_to_linestring(&self.obj.nodes);
                    ring.close();
                    let mut mp = MultiPolygon::new(vec![Polygon::new(ring, vec![])]);
                    tilemaker_geom::valid::orient_multipolygon(&mut mp);
                    mp
                }
            };
            correct(&mut mp);
            self.polygon = Some(Arc::new(mp));
        }
        self.polygon.clone().unwrap()
    }

    fn multilinestring(&mut self) -> Arc<MultiLineString> {
        if self.lines.is_none() {
            let ways: Vec<&[LatpLon]> = self.obj.outers.iter().map(|w| w.as_slice()).collect();
            self.lines = Some(Arc::new(multilinestring_from_ways(&ways)));
        }
        self.lines.clone().unwrap()
    }

    fn push_geometry(
        &mut self,
        kind: GeometryKind,
        make: impl FnOnce(&mut Self) -> Option<OutputGeometry>,
    ) -> Option<usize> {
        if let Some(&g) = self.by_kind.get(&kind) {
            return g;
        }
        let g = make(self).map(|geom| {
            self.geometries.push(geom);
            self.geometries.len() - 1
        });
        self.by_kind.insert(kind, g);
        g
    }

    fn layer_index(&self, name: &str, func: &str) -> mlua::Result<usize> {
        self.shared
            .layers
            .index(name)
            .ok_or_else(|| mlua::Error::runtime(format!("{func}(): a layer named as \"{name}\" doesn't exist.")))
    }

    fn layer(&mut self, name: &str, area: bool) -> mlua::Result<()> {
        let layer = self.layer_index(name, "Layer")?;
        let kind = match self.obj.kind {
            ObjectKind::Node => GeometryKind::Point,
            ObjectKind::Way if area => GeometryKind::Polygon,
            ObjectKind::Way => GeometryKind::LineString,
            ObjectKind::Relation if area => GeometryKind::Polygon,
            ObjectKind::Relation => GeometryKind::MultiLineString,
        };
        let geometry = self.push_geometry(kind, |c| match kind {
            GeometryKind::Point => c.obj.location.map(OutputGeometry::Point),
            GeometryKind::LineString => {
                (c.obj.nodes.len() >= 2).then(|| OutputGeometry::LineString(c.obj.nodes.as_slice().into()))
            }
            GeometryKind::Polygon => {
                if c.obj.kind == ObjectKind::Way && c.obj.nodes.len() < 3 {
                    return None;
                }
                let mp = c.multipolygon();
                (!mp.0.is_empty()).then(|| OutputGeometry::MultiPolygon(Arc::new(CompactPolygons::from_geo(&mp))))
            }
            GeometryKind::MultiLineString => {
                let mls = c.multilinestring();
                (!mls.0.is_empty()).then(|| OutputGeometry::MultiLineString(Arc::new(CompactLines::from_geo(&mls))))
            }
        });
        // An object whose geometry is unusable writes nothing, but later
        // Attribute() calls must not land on the previous output.
        let Some(geometry) = geometry else {
            self.outputs.push(PendingOutput {
                layer: u8::MAX,
                geometry: usize::MAX,
                min_zoom: 0,
                z_order: 0.0,
                attributes: Vec::new(),
            });
            return Ok(());
        };
        self.outputs.push(PendingOutput {
            layer: layer as u8,
            geometry,
            min_zoom: self.shared.layers.layers[layer].minzoom,
            z_order: 0.0,
            attributes: Vec::new(),
        });
        Ok(())
    }

    fn centroid_of(&mut self, polylabel: bool) -> Option<Point> {
        match self.obj.kind {
            ObjectKind::Node => self.obj.location.map(|l| Point::from(ll_to_coord(l))),
            ObjectKind::Way if !self.obj.is_closed() => measure::linestring_centroid(&self.linestring()),
            ObjectKind::Way | ObjectKind::Relation => {
                if self.obj.kind == ObjectKind::Relation && !self.obj.closed {
                    let mls = self.multilinestring();
                    return geo::Centroid::centroid(&*mls);
                }
                let mp = self.multipolygon();
                if polylabel {
                    measure::multipolygon_polylabel(&mp).or_else(|| measure::multipolygon_centroid(&mp))
                } else {
                    measure::multipolygon_centroid(&mp)
                }
            }
        }
    }

    fn layer_as_centroid(&mut self, name: &str, args: &[String]) -> mlua::Result<()> {
        let layer = self.layer_index(name, "LayerAsCentroid")?;
        let polylabel = match args.first().map(String::as_str) {
            None | Some("polylabel") => true,
            Some("centroid") => false,
            Some(other) => return Err(mlua::Error::runtime(format!("unknown centroid algorithm {other}"))),
        };
        let mut point = None;
        if self.obj.kind == ObjectKind::Relation {
            'roles: for role in args.iter().skip(1) {
                for (r, loc) in &self.obj.node_members {
                    if r == role {
                        point = Some(*loc);
                        break 'roles;
                    }
                }
            }
        }
        let point = match point {
            Some(p) => Some(p),
            None => self.centroid_of(polylabel).map(|p| LatpLon {
                latp: (p.y() * 1e7).round() as i32,
                lon: (p.x() * 1e7).round() as i32,
            }),
        };
        let Some(point) = point else {
            self.outputs.push(PendingOutput {
                layer: u8::MAX,
                geometry: usize::MAX,
                min_zoom: 0,
                z_order: 0.0,
                attributes: Vec::new(),
            });
            return Ok(());
        };
        self.geometries.push(OutputGeometry::Point(point));
        let geometry = self.geometries.len() - 1;
        self.outputs.push(PendingOutput {
            layer: layer as u8,
            geometry,
            min_zoom: self.shared.layers.layers[layer].minzoom,
            z_order: 0.0,
            attributes: Vec::new(),
        });
        Ok(())
    }

    fn attribute(&mut self, func: &str, key: String, value: AttributeValue, min_zoom: u8, field: u8) {
        let Some(out) = self.outputs.last_mut() else {
            self.warn(format!(
                "{func}(\"{key}\") without a Layer() first (object {})",
                self.obj.id
            ));
            return;
        };
        if out.layer == u8::MAX {
            return;
        }
        out.attributes.retain(|a| a.key != key);
        let layer = out.layer as usize;
        out.attributes.push(Attribute {
            key: key.clone(),
            value,
            min_zoom,
        });
        // Shared by every Lua state: lock only for keys this state has not
        // recorded yet (as tilemaker's thread-local "already seen" set).
        if self.seen_fields.insert((layer as u8, key.clone())) {
            let mut fields = self.shared.fields.lock().unwrap();
            fields[layer].entry(key).or_insert(field);
        }
    }

    fn area(&mut self) -> f64 {
        if !self.obj.is_closed() || (self.obj.kind == ObjectKind::Way && self.obj.nodes.len() < 4) {
            return 0.0;
        }
        measure::multipolygon_area_m2(&self.multipolygon())
    }

    fn length(&mut self) -> f64 {
        if self.obj.kind == ObjectKind::Way {
            measure::linestring_length_m(&self.linestring())
        } else {
            0.0
        }
    }
}

/// Make a multipolygon valid if its rings cross (tilemaker's `make_valid`).
fn correct(mp: &mut MultiPolygon) {
    let crossing = mp.0.iter().any(|p| {
        let segs: Vec<(LatpLon, LatpLon)> = std::iter::once(p.exterior())
            .chain(p.interiors())
            .flat_map(|r| {
                r.0.windows(2).map(|w| {
                    let f = |c: tilemaker_geom::Coord| LatpLon {
                        latp: (c.y * 1e7).round() as i32,
                        lon: (c.x * 1e7).round() as i32,
                    };
                    (f(w[0]), f(w[1]))
                })
            })
            .collect();
        tilemaker_geom::assemble::has_crossings(&segs)
    });
    if crossing {
        tilemaker_geom::valid::repair_multipolygon(mp);
    }
}

fn relations_with_parents(store: &ScannedRelations, id: i64) -> Vec<(i64, String)> {
    let mut out = Vec::new();
    let mut todo = vec![id];
    let mut done = std::collections::HashSet::new();
    while let Some(r) = todo.pop() {
        if !done.insert(r) {
            continue;
        }
        for (parent, role) in store.relations_for(MemberKind::Relation, r) {
            todo.push(parent);
            out.push((parent, role));
        }
    }
    out
}

fn min_zoom_arg(v: Option<f64>) -> u8 {
    v.map(|z| z.clamp(0.0, 255.0) as u8).unwrap_or(0)
}

fn with<R>(lua: &Lua, f: impl FnOnce(&mut Context) -> R) -> R {
    let mut c = lua.app_data_mut::<Context>().expect("profile context");
    f(&mut c)
}

/// Register the global API functions.
pub(crate) fn register(lua: &Lua) -> mlua::Result<()> {
    let g = lua.globals();

    g.set(
        "Find",
        lua.create_function(|lua, key: mlua::LuaString| {
            let c = lua.app_data_ref::<Context>().unwrap();
            lua.create_string(c.find(&key.as_bytes()).unwrap_or(""))
        })?,
    )?;
    g.set(
        "Holds",
        lua.create_function(|lua, key: mlua::LuaString| {
            let c = lua.app_data_ref::<Context>().unwrap();
            Ok(c.find(&key.as_bytes()).is_some())
        })?,
    )?;
    g.set(
        "HasTags",
        lua.create_function(|lua, ()| Ok(!lua.app_data_ref::<Context>().unwrap().obj.tags.is_empty()))?,
    )?;
    g.set(
        "AllKeys",
        lua.create_function(|lua, ()| {
            let keys: Vec<String> = lua
                .app_data_ref::<Context>()
                .unwrap()
                .obj
                .tags
                .iter()
                .map(|t| t.0.clone())
                .collect();
            lua.create_sequence_from(keys)
        })?,
    )?;
    g.set(
        "AllTags",
        lua.create_function(|lua, ()| {
            let tags = lua.app_data_ref::<Context>().unwrap().obj.tags.clone();
            lua.create_table_from(tags)
        })?,
    )?;
    g.set(
        "Id",
        lua.create_function(|lua, ()| Ok(lua.app_data_ref::<Context>().unwrap().id.to_string()))?,
    )?;
    g.set(
        "OsmType",
        lua.create_function(|lua, ()| {
            Ok(match lua.app_data_ref::<Context>().unwrap().obj.kind {
                ObjectKind::Node => "node",
                ObjectKind::Way => "way",
                ObjectKind::Relation => "relation",
            })
        })?,
    )?;
    g.set(
        "IsClosed",
        lua.create_function(|lua, ()| Ok(lua.app_data_ref::<Context>().unwrap().obj.is_closed()))?,
    )?;
    g.set(
        "IsMultiPolygon",
        lua.create_function(|lua, ()| Ok(lua.app_data_ref::<Context>().unwrap().obj.kind == ObjectKind::Relation))?,
    )?;
    g.set("Area", lua.create_function(|lua, ()| Ok(with(lua, |c| c.area())))?)?;
    g.set("Length", lua.create_function(|lua, ()| Ok(with(lua, |c| c.length())))?)?;
    g.set(
        "Centroid",
        lua.create_function(|lua, algo: Option<String>| {
            let polylabel = !matches!(algo.as_deref(), Some("centroid"));
            match with(lua, |c| c.centroid_of(polylabel)) {
                Some(p) => {
                    let t = lua.create_table()?;
                    t.set(1, latp2lat(p.y()))?;
                    t.set(2, p.x())?;
                    Ok(Value::Table(t))
                }
                None => Ok(Value::Nil),
            }
        })?,
    )?;
    g.set(
        "Layer",
        lua.create_function(|lua, (name, area): (String, Option<bool>)| {
            with(lua, |c| c.layer(&name, area.unwrap_or(false)))
        })?,
    )?;
    g.set(
        "LayerAsCentroid",
        lua.create_function(|lua, (name, args): (String, Variadic<String>)| {
            let args: Vec<String> = args.into_iter().collect();
            with(lua, |c| c.layer_as_centroid(&name, &args))
        })?,
    )?;
    g.set(
        "Attribute",
        lua.create_function(|lua, (key, value, mz): (String, Value, Option<f64>)| {
            let text = match &value {
                Value::String(s) => s.to_string_lossy(),
                Value::Integer(i) => i.to_string(),
                Value::Number(n) => lua_number_string(*n),
                Value::Boolean(b) => b.to_string(),
                Value::Nil => {
                    with(lua, |c| {
                        c.shared
                            .warn(format!("Attribute(\"{key}\") given nil (object {})", c.obj.id))
                    });
                    return Ok(());
                }
                other => {
                    return Err(mlua::Error::runtime(format!(
                        "Attribute(\"{key}\"): cannot use a {}",
                        other.type_name()
                    )))
                }
            };
            with(lua, |c| {
                c.attribute("Attribute", key, AttributeValue::String(text), min_zoom_arg(mz), 0)
            });
            Ok(())
        })?,
    )?;
    g.set(
        "AttributeNumeric",
        lua.create_function(|lua, (key, value, mz): (String, f64, Option<f64>)| {
            with(lua, |c| {
                c.attribute(
                    "AttributeNumeric",
                    key,
                    AttributeValue::Float(value),
                    min_zoom_arg(mz),
                    1,
                )
            });
            Ok(())
        })?,
    )?;
    g.set(
        "AttributeInteger",
        lua.create_function(|lua, (key, value, mz): (String, f64, Option<f64>)| {
            with(lua, |c| {
                c.attribute(
                    "AttributeInteger",
                    key,
                    AttributeValue::Int(value as i64),
                    min_zoom_arg(mz),
                    1,
                )
            });
            Ok(())
        })?,
    )?;
    g.set(
        "AttributeBoolean",
        lua.create_function(|lua, (key, value, mz): (String, bool, Option<f64>)| {
            with(lua, |c| {
                c.attribute(
                    "AttributeBoolean",
                    key,
                    AttributeValue::Bool(value),
                    min_zoom_arg(mz),
                    2,
                )
            });
            Ok(())
        })?,
    )?;
    g.set(
        "MinZoom",
        lua.create_function(|lua, z: f64| {
            with(lua, |c| match c.outputs.last_mut() {
                Some(o) => o.min_zoom = z.clamp(0.0, 255.0) as u8,
                None => c
                    .shared
                    .warn(format!("MinZoom() without a Layer() first (object {})", c.obj.id)),
            });
            Ok(())
        })?,
    )?;
    g.set(
        "ZOrder",
        lua.create_function(|lua, z: f64| {
            with(lua, |c| match c.outputs.last_mut() {
                Some(o) => o.z_order = z as f32,
                None => c
                    .shared
                    .warn(format!("ZOrder() without a Layer() first (object {})", c.obj.id)),
            });
            Ok(())
        })?,
    )?;
    // Decluttering is not implemented: scores are accepted and ignored.
    g.set("Score", lua.create_function(|_, _score: f64| Ok(()))?)?;
    g.set(
        "ModifyId",
        lua.create_function(|lua, id: i64| {
            with(lua, |c| c.id = id);
            Ok(())
        })?,
    )?;
    g.set(
        "Accept",
        lua.create_function(|lua, ()| {
            with(lua, |c| c.accepted = true);
            Ok(())
        })?,
    )?;
    g.set(
        "NextRelation",
        lua.create_function(|lua, ()| {
            with(lua, |c| {
                c.relation_cursor += 1;
                match c.obj.relations.get(c.relation_cursor as usize) {
                    Some((id, role)) => {
                        let mut v = MultiValue::new();
                        v.push_back(Value::Integer(*id as _));
                        v.push_back(Value::String(lua.create_string(role)?));
                        Ok(v)
                    }
                    None => Ok(MultiValue::new()),
                }
            })
        })?,
    )?;
    g.set(
        "RestartRelations",
        lua.create_function(|lua, ()| {
            with(lua, |c| c.relation_cursor = -1);
            Ok(())
        })?,
    )?;
    g.set(
        "FindInRelation",
        lua.create_function(|lua, key: String| {
            with(lua, |c| {
                let Some(&(rel, _)) = c.obj.relations.get(c.relation_cursor.max(0) as usize) else {
                    return Ok(String::new());
                };
                let v = match &c.postscan {
                    Some(p) => p.tag(rel, &key),
                    None => c.scanned.tag(rel, &key),
                };
                Ok(v.unwrap_or("").to_string())
            })
        })?,
    )?;
    g.set(
        "SetTag",
        lua.create_function(|lua, (key, value): (String, String)| {
            with(lua, |c| {
                let id = c.obj.id;
                match &mut c.postscan {
                    Some(p) => {
                        p.set_tag(id, &key, &value);
                        Ok(())
                    }
                    None => Err(mlua::Error::runtime(
                        "SetTag can only be used in relation_postscan_function",
                    )),
                }
            })
        })?,
    )?;
    g.set(
        "SetData",
        lua.create_function(|lua, (key, value): (String, String)| {
            with(lua, |c| c.shared.data.lock().unwrap().insert(key, value));
            Ok(())
        })?,
    )?;
    g.set(
        "GetData",
        lua.create_function(|lua, key: String| {
            Ok(with(lua, |c| {
                c.shared.data.lock().unwrap().get(&key).cloned().unwrap_or_default()
            }))
        })?,
    )?;
    for name in ["Intersects", "CoveredBy"] {
        g.set(
            name,
            lua.create_function(move |lua, _layer: String| {
                with(lua, |c| {
                    c.warn(format!(
                        "{name}() always returns false: shapefile layers are not supported"
                    ))
                });
                Ok(false)
            })?,
        )?;
    }
    for name in ["FindIntersecting", "FindCovering"] {
        g.set(
            name,
            lua.create_function(move |lua, _layer: String| {
                with(lua, |c| {
                    c.warn(format!(
                        "{name}() always returns nothing: shapefile layers are not supported"
                    ))
                });
                lua.create_table()
            })?,
        )?;
    }
    g.set(
        "AreaIntersecting",
        lua.create_function(|lua, _layer: String| {
            with(lua, |c| {
                c.shared
                    .warn("AreaIntersecting() always returns 0: shapefile layers are not supported".into())
            });
            Ok(0.0)
        })?,
    )?;
    Ok(())
}

/// Lua's `tostring` for numbers: integers without a fraction.
fn lua_number_string(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}
