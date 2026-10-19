import { describe, expect, it } from "vitest";
import { pathToFileURL } from "node:url";

import { openPbf, OsmStreamError, version } from "../ts/index.js";
import { fixture } from "./helpers.js";

describe("openPbf", () => {
  it("reads the header", async () => {
    const pbf = await openPbf(fixture("town.osm.pbf"));
    expect(pbf.header).toEqual({
      bbox: [-1, -1, 11, 11],
      requiredFeatures: ["OsmSchema-V0.6", "DenseNodes"],
      optionalFeatures: ["Sort.Type_then_ID"],
      writingProgram: "make_test_fixtures",
      source: null,
      replicationTimestamp: null,
      replicationSequenceNumber: null,
      replicationBaseUrl: null,
      sorted: true,
      locationsOnWays: false,
    });
    expect(pbf.blockCount).toBe(3);
    expect(pbf.size).toBe(798);
    expect(pbf.path).toBe(fixture("town.osm.pbf"));
  });

  it("accepts file URLs", async () => {
    const pbf = await openPbf(pathToFileURL(fixture("town.osm.pbf")));
    expect(pbf.blockCount).toBe(3);
  });

  it("rejects missing files", async () => {
    await expect(openPbf(fixture("nope.osm.pbf"))).rejects.toBeInstanceOf(OsmStreamError);
    await expect(openPbf(fixture("nope.osm.pbf"))).rejects.toThrow(/nope\.osm\.pbf/);
  });

  it("rejects files that are not PBF", async () => {
    await expect(openPbf(fixture("not-a-pbf.osm.pbf"))).rejects.toThrow(/corrupt PBF data/);
  });

  it("reports LocationsOnWays", async () => {
    const pbf = await openPbf(fixture("low.osm.pbf"));
    expect(pbf.header.locationsOnWays).toBe(true);
    expect(pbf.header.bbox).toBeNull();
  });

  it("has a version", () => {
    expect(version()).toMatch(/^\d+\.\d+\.\d+$/);
  });
});
