//! tilemaker's Lua profile API (`process.lua`), on mlua.
//!
//! A [`LuaProfile`] owns one Lua state; tilemaker runs one per thread. For
//! each OSM object the host fills an [`Object`], calls
//! [`LuaProfile::process`], and gets back a [`ProcessedObject`]: the
//! geometries and the `Layer()` outputs with their attributes.
//!
//! The global functions match tilemaker's (see its `docs/CONFIGURATION.md`):
//! `Find`, `Holds`, `HasTags`, `AllKeys`, `AllTags`, `Id`, `OsmType`,
//! `IsClosed`, `IsMultiPolygon`, `Area`, `Length`, `Centroid`, `Layer`,
//! `LayerAsCentroid`, `Attribute`, `AttributeNumeric`, `AttributeInteger`,
//! `AttributeBoolean`, `MinZoom`, `ZOrder`, `Score`, `ModifyId`, `Accept`,
//! `NextRelation`, `RestartRelations`, `FindInRelation`, `SetTag`,
//! `SetData`, `GetData`. Shapefile queries (`Intersects`, `FindIntersecting`,
//! `CoveredBy`, `FindCovering`, `AreaIntersecting`) are accepted but always
//! report nothing, as external shapefile/GeoJSON layers are not supported.

mod context;
mod output;
mod relations;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mlua::{Function, Lua, Value};

use context::Context;
pub use context::{Object, ObjectKind};
pub use output::{Attribute, AttributeValue, GeometryKind, OutputGeometry, PendingOutput, ProcessedObject};
pub use relations::{MemberKind, ScannedRelations};

/// A profile error (with the Lua traceback when there is one).
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// Loading or running Lua failed.
    #[error("Lua error{context}: {source}")]
    Lua {
        /// Which object, if any.
        context: String,
        /// The Lua error.
        #[source]
        source: mlua::Error,
    },
}

fn lua_err(context: impl Into<String>) -> impl FnOnce(mlua::Error) -> ProfileError {
    let context = context.into();
    move |source| ProfileError::Lua { context, source }
}

/// Things every profile instance shares: layer definitions, `SetData`
/// storage, and the attribute types seen per layer (for `vector_layers`).
#[derive(Debug)]
pub struct Shared {
    /// Layers from the config.
    pub layers: tilemaker_config::LayerDefinition,
    /// The config's name (passed to `init_function`).
    pub project_name: String,
    /// `SetData`/`GetData` storage.
    pub data: Mutex<HashMap<String, String>>,
    /// Per layer: attribute key → 0 string, 1 number, 2 boolean.
    pub fields: Mutex<Vec<HashMap<String, u8>>>,
    /// Warnings (bad calls in the profile), capped.
    pub warnings: Mutex<Vec<String>>,
}

impl Shared {
    /// From a config.
    pub fn new(config: &tilemaker_config::Config) -> Arc<Self> {
        Arc::new(Self {
            layers: config.layers.clone(),
            project_name: config.name.clone(),
            data: Mutex::new(HashMap::new()),
            fields: Mutex::new(vec![HashMap::new(); config.layers.layers.len()]),
            warnings: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn warn(&self, msg: String) {
        let mut w = self.warnings.lock().unwrap();
        if w.len() < 100 && !w.contains(&msg) {
            w.push(msg);
        }
    }
}

/// What a profile defines.
#[derive(Clone, Debug, Default)]
pub struct Capabilities {
    /// `node_function`.
    pub nodes: bool,
    /// `way_function`.
    pub ways: bool,
    /// `relation_function`.
    pub relations: bool,
    /// `relation_scan_function`.
    pub relation_scan: bool,
    /// `relation_postscan_function`.
    pub relation_postscan: bool,
    /// `node_keys`: only nodes with one of these keys are processed.
    pub node_keys: Option<Vec<String>>,
    /// `way_keys`: only ways with one of these keys (or used by relations).
    pub way_keys: Option<Vec<String>>,
}

/// One Lua state running a profile.
pub struct LuaProfile {
    lua: Lua,
    caps: Capabilities,
    node_fn: Option<Function>,
    way_fn: Option<Function>,
    relation_fn: Option<Function>,
    scan_fn: Option<Function>,
    postscan_fn: Option<Function>,
    compat: Value,
}

impl LuaProfile {
    /// Load a profile's source and call its `init_function(name, is_first)`.
    pub fn load(source: &str, chunk_name: &str, shared: Arc<Shared>, is_first: bool) -> Result<Self, ProfileError> {
        let lua = Lua::new();
        lua.set_app_data(Context::new(shared.clone()));
        context::register(&lua).map_err(lua_err(" registering the API"))?;
        // Lua 5.1 profiles (tilemaker's usual LuaJIT) and 5.2+ helpers.
        lua.load(
            "if unpack == nil then unpack = table.unpack end if table.unpack == nil then table.unpack = unpack end",
        )
        .exec()
        .map_err(lua_err(""))?;
        lua.load(source)
            .set_name(format!("@{chunk_name}"))
            .exec()
            .map_err(lua_err(format!(" loading {chunk_name}")))?;
        let g = lua.globals();
        let f = |name: &str| -> Option<Function> { g.get::<Option<Function>>(name).ok().flatten() };
        let keys = |name: &str| -> Option<Vec<String>> {
            match g.get::<Value>(name).ok()? {
                Value::Table(t) => Some(t.sequence_values::<String>().filter_map(Result::ok).collect()),
                _ => None,
            }
        };
        let caps = Capabilities {
            nodes: f("node_function").is_some(),
            ways: f("way_function").is_some(),
            relations: f("relation_function").is_some(),
            relation_scan: f("relation_scan_function").is_some(),
            relation_postscan: f("relation_postscan_function").is_some(),
            node_keys: keys("node_keys"),
            way_keys: keys("way_keys"),
        };
        if g.get::<Option<Function>>("attribute_function").ok().flatten().is_some() {
            shared.warn("attribute_function is ignored: shapefile/GeoJSON layers are not supported".into());
        }
        // Old-style profiles call methods on their argument (`way:Find(k)`):
        // forward any method to the global function of that name.
        let compat: Value = lua
            .load(
                "return setmetatable({}, { __index = function(_, name) \
                   local f = _G[name]; if f == nil then return nil end \
                   return function(_, ...) return f(...) end end })",
            )
            .eval()
            .map_err(lua_err(""))?;
        if let Some(init) = f("init_function") {
            init.call::<()>((shared.project_name.clone(), is_first))
                .map_err(lua_err(" in init_function"))?;
        }
        Ok(Self {
            node_fn: f("node_function"),
            way_fn: f("way_function"),
            relation_fn: f("relation_function"),
            scan_fn: f("relation_scan_function"),
            postscan_fn: f("relation_postscan_function"),
            compat,
            caps,
            lua,
        })
    }

    /// What the profile defines.
    pub fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    /// Give the profile the relations accepted during the scan (read-only
    /// from here on).
    pub fn set_scanned_relations(&self, relations: Arc<ScannedRelations>) {
        if let Some(mut c) = self.lua.app_data_mut::<Context>() {
            c.scanned = relations;
        }
    }

    /// Run `relation_scan_function` for a relation; whether it called
    /// `Accept()`.
    pub fn scan_relation(&mut self, id: i64, tags: Vec<(String, String)>) -> Result<bool, ProfileError> {
        let Some(f) = &self.scan_fn else { return Ok(false) };
        self.lua.app_data_mut::<Context>().unwrap().begin(Object {
            kind: ObjectKind::Relation,
            id,
            tags,
            ..Object::default()
        });
        f.call::<()>(self.compat.clone())
            .map_err(lua_err(format!(" scanning relation {id}")))?;
        let c = self.lua.app_data_ref::<Context>().unwrap();
        Ok(c.accepted)
    }

    /// Run `relation_postscan_function` for each relation in `ids`, which
    /// may change their tags with `SetTag`. Returns the updated store.
    pub fn postscan(&mut self, relations: ScannedRelations, ids: &[i64]) -> Result<ScannedRelations, ProfileError> {
        let Some(f) = self.postscan_fn.clone() else {
            return Ok(relations);
        };
        self.lua.app_data_mut::<Context>().unwrap().postscan = Some(relations);
        for &id in ids {
            let tags = {
                let c = self.lua.app_data_ref::<Context>().unwrap();
                c.postscan
                    .as_ref()
                    .unwrap()
                    .tags(id)
                    .map(|t| t.to_vec())
                    .unwrap_or_default()
            };
            self.lua.app_data_mut::<Context>().unwrap().begin(Object {
                kind: ObjectKind::Relation,
                id,
                tags,
                ..Object::default()
            });
            f.call::<()>(self.compat.clone())
                .map_err(lua_err(format!(" in relation_postscan_function for relation {id}")))?;
        }
        Ok(self.lua.app_data_mut::<Context>().unwrap().postscan.take().unwrap())
    }

    /// Run the node, way or relation function for an object.
    pub fn process(&mut self, object: Object) -> Result<ProcessedObject, ProfileError> {
        let (f, what) = match object.kind {
            ObjectKind::Node => (&self.node_fn, "node"),
            // tilemaker hands multipolygons to way_function.
            ObjectKind::Way => (&self.way_fn, "way"),
            ObjectKind::Relation if object.multipolygon => (&self.way_fn, "relation"),
            ObjectKind::Relation => (&self.relation_fn, "relation"),
        };
        let Some(f) = f else {
            return Ok(ProcessedObject::default());
        };
        let id = object.id;
        self.lua.app_data_mut::<Context>().unwrap().begin(object);
        f.call::<()>(self.compat.clone())
            .map_err(lua_err(format!(" processing {what} {id}")))?;
        Ok(self.lua.app_data_mut::<Context>().unwrap().finish())
    }

    /// Call `exit_function`, if any.
    pub fn exit(&self) -> Result<(), ProfileError> {
        if let Ok(Some(f)) = self.lua.globals().get::<Option<Function>>("exit_function") {
            f.call::<()>(()).map_err(lua_err(" in exit_function"))?;
        }
        Ok(())
    }
}
