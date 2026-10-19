# Compatibility with tilemaker

What carries over from the C++ tilemaker, and what does not.

## Command line

| tilemaker | here |
|---|---|
| `tilemaker in.osm.pbf out.mbtiles` (positional) | same |
| `--input`, `--output` | same |
| `--config` (default `config.json`), `--process` (default `process.lua`) | same |
| `--bbox minlon,minlat,maxlon,maxlat` | same |
| `--threads N` | same (0: all cores) |
| `--quiet`, `--verbose` | same (`--verbose` prints profile warnings) |
| `--merge` | not supported (rejected) |
| `--store`, `--fast`, `--compact`, `--no-compress-nodes/ways`, `--materialize-geometries`, `--shard-stores` | not needed / not supported: stores are always compressed in memory |
| `--skip-integrity`, `--log-tile-timings` | not supported |
| output `.mbtiles`/`.sqlite`, `.pmtiles`, directory | same |
| multiple `--input` files | one input |

Extra subcommands: `tilemaker export` (filtered features as GeoJSON lines)
and `tilemaker inspect` (feature counts per zoom and layer).

## config.json

Supported: `settings.minzoom`, `maxzoom`, `basezoom`, `include_ids`,
`high_resolution`, `compress` (`gzip`/`deflate`/`none`), `combine_below`,
`mvt_version`, `name`, `version`, `description`, `bounding_box`,
`default_view`, `filemetadata`; per layer `minzoom`, `maxzoom`, `write_to`,
`simplify_below`, `simplify_level`, `simplify_length`, `simplify_ratio`,
`simplify_algorithm` (`visvalingam`; `buildings` falls back to
Douglas–Peucker), `filter_below`, `filter_area`, `feature_limit`,
`feature_limit_below`, `z_order_ascending`, `combine_points`,
`combine_lines_below`, `combine_polygons_below`.

Not supported: `source`, `source_columns`, `index`, `index_column`
(shapefile/GeoJSON layers stay empty, with a warning), `declutter_below`,
`declutter_distance`, `declutter_threshold` (accepted, not applied).

## process.lua

Profiles run on LuaJIT (or Lua 5.4 with the `lua54` feature). Old-style
profiles that call methods on their argument (`way:Find("highway")`) work
too.

| function | |
|---|---|
| `node_keys`, `way_keys` (incl. `key=value` and `~key`) | ✓ |
| `init_function(name, isFirst)`, `exit_function()` | ✓ (per Lua state, as tilemaker) |
| `node_function`, `way_function`, `relation_function` | ✓ |
| `relation_scan_function`, `relation_postscan_function` | ✓ |
| `Find`, `Holds`, `HasTags`, `AllKeys`, `AllTags`, `Id`, `OsmType` | ✓ |
| `IsClosed`, `IsMultiPolygon`, `Area`, `Length`, `Centroid(algorithm)` | ✓ |
| `Layer(name, isArea)`, `LayerAsCentroid(name, algorithm, roles…)` | ✓ |
| `Attribute`, `AttributeNumeric`, `AttributeInteger`, `AttributeBoolean` (with minzoom) | ✓ |
| `MinZoom`, `ZOrder` | ✓ |
| `ModifyId`, `Accept`, `SetTag`, `NextRelation`, `RestartRelations`, `FindInRelation` | ✓ |
| `SetData`, `GetData` | ✓ |
| `Score` | accepted, ignored (no decluttering) |
| `Intersects`, `FindIntersecting`, `CoveredBy`, `FindCovering`, `AreaIntersecting` | report nothing (no shapefile layers), with a warning |
| `attribute_function` | ignored, with a warning |

## Output

Same tiles for the same input, within numerical differences: on Monaco
and Liechtenstein with the OpenMapTiles profile, per zoom and layer feature
counts are within 1% of the C++ build's (most layers identical). The
remaining differences are single features on either side of a tile edge
(label points from polylabel, polygon clipping slivers). Empty tiles are
not written. MBTiles metadata (`json` with `vector_layers`, `bounds`,
`center`, …) and the directory's `metadata.json` follow tilemaker's.
