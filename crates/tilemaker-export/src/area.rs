//! When a closed way is an area.

use tilemaker_pbf::Tags;

/// What closed ways become.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClosedWays {
    /// Decide from the tags (`area=yes/no`, then area-like keys; see
    /// [`is_area_by_tags`]): a polygon or a linestring.
    #[default]
    Auto,
    /// Always a polygon.
    Polygon,
    /// Always a linestring.
    LineString,
    /// Both a polygon and a linestring (osmium export's default).
    Both,
}

impl ClosedWays {
    /// (polygon, linestring) for a closed way with these tags.
    pub fn outputs(self, tags: &Tags<'_>) -> (bool, bool) {
        match self {
            ClosedWays::Polygon => (true, false),
            ClosedWays::LineString => (false, true),
            ClosedWays::Both => (true, true),
            ClosedWays::Auto => {
                let a = is_area_by_tags(tags);
                (a, !a)
            }
        }
    }
}

/// Whether a closed way's tags make it an area: `area=yes` (or `no`)
/// decides; otherwise keys that describe surfaces do (the same rules as iD
/// and osm2pgsql's defaults, simplified).
pub fn is_area_by_tags(tags: &Tags<'_>) -> bool {
    match tags.get("area") {
        Some("yes") => return true,
        Some("no") => return false,
        _ => {}
    }
    for (k, v) in tags.iter() {
        let area = match k {
            "building" | "building:part" | "landuse" | "leisure" | "amenity" | "shop" | "tourism" | "historic"
            | "military" | "office" | "craft" | "place" | "water" | "wetland" | "golf" | "indoor" | "area:highway"
            | "landcover" | "healthcare" | "club" | "emergency" => true,
            "natural" => !matches!(
                v,
                "coastline" | "cliff" | "ridge" | "arete" | "tree_row" | "valley" | "gorge"
            ),
            "waterway" => matches!(v, "riverbank" | "dock" | "boatyard" | "dam"),
            "man_made" => !matches!(
                v,
                "pier" | "breakwater" | "cutline" | "embankment" | "pipeline" | "dyke" | "groyne"
            ),
            "aeroway" => !matches!(v, "runway" | "taxiway" | "parking_position"),
            "power" => matches!(v, "plant" | "substation" | "generator" | "transformer"),
            "railway" => matches!(v, "platform" | "station" | "turntable" | "roundhouse"),
            "public_transport" => matches!(v, "platform" | "station"),
            "highway" => matches!(v, "services" | "rest_area"),
            _ => false,
        };
        if area {
            return true;
        }
    }
    false
}
