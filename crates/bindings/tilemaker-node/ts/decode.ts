// Columnar native batches → objects. Coordinates, refs and parts are
// subarray views into the batch's typed arrays (no copying).

import type { NativeElementBatch, NativeFeatureBatch, NativeMeta, NativeTileBatch } from "./native.js";
import type { Element, Feature, Geometry, Member, Meta, OsmType, ParentRelation, Tags, Tile } from "./types.js";

const OSM_TYPES: readonly OsmType[] = ["node", "way", "relation"];

function tagsAt(strings: string[], offsets: Uint32Array, pairs: Uint32Array, i: number): Tags {
  const tags: Tags = {};
  const end = offsets[i + 1]!;
  for (let k = offsets[i]!; k < end; k += 2) {
    const key = strings[pairs[k]!]!;
    const value = strings[pairs[k + 1]!]!;
    if (key === "__proto__") {
      Object.defineProperty(tags, key, { value, enumerable: true, writable: true, configurable: true });
    } else {
      tags[key] = value;
    }
  }
  return tags;
}

function metaAt(strings: string[], m: NativeMeta | undefined, i: number): Meta | undefined {
  if (!m || !m.present[i]) return undefined;
  return {
    version: m.version[i]!,
    timestamp: m.timestamp[i]!,
    changeset: m.changeset[i]!,
    uid: m.uid[i]!,
    user: strings[m.user[i]!]!,
    visible: m.visible[i] === 1,
  };
}

export function decodeFeatures(b: NativeFeatureBatch): Feature[] {
  const n = b.types.length;
  const out = new Array<Feature>(n);
  const m = b.memberOf;
  // Each parent relation's tags, decoded once per batch.
  const parentTags: Tags[] = m ? Array.from(m.ids, (_, r) => tagsAt(b.strings, m.tagOffsets, m.tags, r)) : [];
  for (let i = 0; i < n; i++) {
    const coordinates = b.coords.subarray(b.coordOffsets[i]!, b.coordOffsets[i + 1]!);
    let geometry: Geometry;
    switch (b.geometryTypes[i]) {
      case 0:
        geometry = { type: "Point", coordinates };
        break;
      case 1:
        geometry = { type: "LineString", coordinates };
        break;
      case 2:
        geometry = {
          type: "MultiLineString",
          coordinates,
          parts: b.parts.subarray(b.partOffsets[i]!, b.partOffsets[i + 1]!),
        };
        break;
      default:
        geometry = {
          type: "MultiPolygon",
          coordinates,
          parts: b.parts.subarray(b.partOffsets[i]!, b.partOffsets[i + 1]!),
          polygons: b.polygons.subarray(b.polygonOffsets[i]!, b.polygonOffsets[i + 1]!),
        };
    }
    const f: Feature = {
      type: OSM_TYPES[b.types[i]!]!,
      id: b.ids[i]!,
      tags: tagsAt(b.strings, b.tagOffsets, b.tags, i),
      geometry,
    };
    const meta = metaAt(b.strings, b.meta, i);
    if (meta) f.meta = meta;
    if (b.area) f.area = b.area[i]!;
    if (b.length) f.length = b.length[i]!;
    if (b.centroid) f.centroid = [b.centroid[2 * i]!, b.centroid[2 * i + 1]!];
    if (b.labelPoint) f.labelPoint = [b.labelPoint[2 * i]!, b.labelPoint[2 * i + 1]!];
    if (b.bbox) f.bbox = [b.bbox[4 * i]!, b.bbox[4 * i + 1]!, b.bbox[4 * i + 2]!, b.bbox[4 * i + 3]!];
    if (m) {
      const start = m.offsets[i]!;
      const end = m.offsets[i + 1]!;
      const parents = new Array<ParentRelation>(end - start);
      for (let k = start; k < end; k++) {
        const r = m.relations[k]!;
        parents[k - start] = { id: m.ids[r]!, role: b.strings[m.roles[k]!]!, tags: parentTags[r]! };
      }
      f.memberOf = parents;
    }
    out[i] = f;
  }
  return out;
}

export function decodeElements(b: NativeElementBatch): Element[] {
  const n = b.types.length;
  const out = new Array<Element>(n);
  for (let i = 0; i < n; i++) {
    const id = b.ids[i]!;
    const tags = tagsAt(b.strings, b.tagOffsets, b.tags, i);
    let e: Element;
    switch (b.types[i]) {
      case 0:
        e = { type: "node", id, tags, lon: b.locations[2 * i]!, lat: b.locations[2 * i + 1]! };
        break;
      case 1:
        e = { type: "way", id, tags, refs: b.refs.subarray(b.refOffsets[i]!, b.refOffsets[i + 1]!) };
        if (b.hasWayCoords[i]) {
          e.coordinates = b.wayCoords.subarray(b.wayCoordOffsets[i]!, b.wayCoordOffsets[i + 1]!);
        }
        break;
      default: {
        const start = b.memberOffsets[i]!;
        const end = b.memberOffsets[i + 1]!;
        const members = new Array<Member>(end - start);
        for (let k = start; k < end; k++) {
          members[k - start] = {
            type: OSM_TYPES[b.memberTypes[k]!]!,
            ref: b.memberIds[k]!,
            role: b.strings[b.memberRoles[k]!]!,
          };
        }
        e = { type: "relation", id, tags, members };
      }
    }
    const meta = metaAt(b.strings, b.meta, i);
    if (meta) e.meta = meta;
    out[i] = e;
  }
  return out;
}

export function decodeTiles(b: NativeTileBatch): Tile[] {
  const n = b.z.length;
  const out = new Array<Tile>(n);
  for (let i = 0; i < n; i++) {
    out[i] = { z: b.z[i]!, x: b.x[i]!, y: b.y[i]!, data: b.data.subarray(b.offsets[i]!, b.offsets[i + 1]!) };
  }
  return out;
}
