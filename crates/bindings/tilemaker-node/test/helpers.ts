import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));

export const fixture = (name: string): string => join(here, "fixtures", name);

/** Real extracts (not committed): skipped when absent. */
export const extract = (name: string): string | undefined => {
  const p = join(here, "..", "..", "..", "..", "fixtures", name);
  return existsSync(p) ? p : undefined;
};

export function signedArea(c: Float64Array, start = 0, end = c.length / 2): number {
  let s = 0;
  for (let i = start; i < end - 1; i++) s += c[2 * i]! * c[2 * i + 3]! - c[2 * i + 2]! * c[2 * i + 1]!;
  return s / 2;
}
