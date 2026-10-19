# tilemaker-core

tilemaker's pipeline in Rust: read an OpenStreetMap PBF file through a Lua
profile (`process.lua`), index the outputs by tile, and render Mapbox
vector tiles per the JSON config — streamed to a callback or written to
MBTiles, PMTiles or a directory (`tilemaker-output`).

Profiles and configs are tilemaker's; the OpenMapTiles profile runs
unchanged and matches the C++ tilemaker's output to within 1% of features
per zoom and layer. See the workspace README for what is not supported.
