// A pull-based stream over a native export. Each batch the consumer takes
// grants the producer one more, so memory stays bounded by `readAhead`
// batches however slow the consumer is.
//
// The native side keeps the event loop alive only while JS awaits a batch
// (or the end of a closed stream): a stream that is abandoned half-read
// does not keep the process running.

import { Readable } from "node:stream";

import type { NativeEvent, NativeStream } from "./native.js";
import { OsmStreamError, type ExportStats, type Progress } from "./types.js";

type Starter = (callback: (event: NativeEvent) => void) => NativeStream | Promise<NativeStream>;

interface Waiter<T> {
  resolve: (batch: T[] | null) => void;
  reject: (error: Error) => void;
}

/**
 * Records streamed out of a PBF file. Iterate it once, record by record
 * (`for await (const f of stream)`) or batch by batch (`stream.batches()`).
 * Nothing is read until iteration starts; breaking out of a loop (or
 * `close()`) stops the export.
 */
export class OsmStream<T, S = ExportStats, P = Progress> implements AsyncIterable<T> {
  /** Resolves with counters when the stream ends (or is closed). */
  readonly stats: Promise<S>;

  #start: Starter;
  #decode: (batch: never) => T[];
  #onProgress: ((p: P) => void) | undefined;
  #signal: AbortSignal | undefined;
  #native: NativeStream | undefined;
  #queue: T[][] = [];
  #waiter: Waiter<T> | undefined;
  #ended = false;
  #closed = false;
  #error: Error | undefined;
  #claimed = false;
  #referenced = true;
  #released = false;
  #emptyStats: (stopped: boolean) => S;
  #resolveStats!: (s: S) => void;
  #rejectStats!: (e: Error) => void;

  /** @internal */
  constructor(
    start: Starter,
    decode: (batch: never) => T[],
    onProgress?: (p: P) => void,
    signal?: AbortSignal,
    makeEmptyStats?: (stopped: boolean) => S,
  ) {
    this.#emptyStats = makeEmptyStats ?? (emptyStats as unknown as (stopped: boolean) => S);
    this.#start = start;
    this.#decode = decode;
    this.#onProgress = onProgress;
    this.#signal = signal;
    this.stats = new Promise<S>((resolve, reject) => {
      this.#resolveStats = resolve;
      this.#rejectStats = reject;
    });
    // Errors surface through iteration; don't also report them as unhandled.
    this.stats.catch(() => undefined);
  }

  /** Keep the event loop alive only while something waits on the native side. */
  #updateRef(): void {
    if (!this.#native || this.#released) return;
    const wanted = this.#waiter !== undefined || (this.#closed && !this.#ended);
    if (wanted !== this.#referenced) {
      this.#referenced = wanted;
      this.#native.setRef(wanted);
    }
  }

  #release(): void {
    if (!this.#native || this.#released) return;
    this.#released = true;
    this.#native.setRef(false);
    this.#native.release();
  }

  #onEvent = (event: NativeEvent): void => {
    switch (event.type) {
      case "batch": {
        let records: T[];
        try {
          records = this.#decode(event.batch as never);
        } catch (e) {
          this.#fail(e instanceof Error ? e : new OsmStreamError(String(e)));
          return;
        }
        if (this.#closed) return;
        if (this.#waiter) {
          const w = this.#waiter;
          this.#waiter = undefined;
          this.#native?.request(1);
          w.resolve(records);
        } else {
          this.#queue.push(records);
        }
        this.#updateRef();
        return;
      }
      case "progress": {
        if (!this.#onProgress) return;
        const { type: _type, ...progress } = event;
        void _type;
        this.#onProgress(progress as unknown as P);
        return;
      }
      case "end":
        this.#ended = true;
        this.#release();
        this.#resolveStats(event.stats as unknown as S);
        this.#wake();
        return;
      case "error":
        this.#fail(new OsmStreamError(event.message));
        return;
    }
  };

  #fail(error: Error): void {
    if (this.#error) return;
    this.#error = error;
    this.#ended = true;
    this.#native?.close();
    this.#release();
    this.#rejectStats(error);
    if (this.#waiter) {
      const w = this.#waiter;
      this.#waiter = undefined;
      w.reject(error);
    }
  }

  #wake(): void {
    if (this.#waiter && this.#queue.length === 0) {
      const w = this.#waiter;
      this.#waiter = undefined;
      if (this.#error) w.reject(this.#error);
      else w.resolve(null);
    }
  }

  #abort = (): void => {
    const reason = this.#signal?.reason;
    this.#fail(reason instanceof Error ? reason : new OsmStreamError("The stream was aborted"));
  };

  async #ensureStarted(): Promise<void> {
    if (this.#native || this.#ended) return;
    if (this.#signal?.aborted) {
      this.#abort();
      return;
    }
    try {
      this.#native = await this.#start(this.#onEvent);
    } catch (e) {
      this.#fail(new OsmStreamError(e instanceof Error ? e.message : String(e)));
      return;
    }
    this.#signal?.addEventListener("abort", this.#abort, { once: true });
    if (this.#closed) this.#native.close();
    this.#updateRef();
  }

  async #next(): Promise<T[] | null> {
    await this.#ensureStarted();
    const queued = this.#queue.shift();
    if (queued) {
      this.#native?.request(1);
      return queued;
    }
    if (this.#error) throw this.#error;
    if (this.#ended || this.#closed) return null;
    const p = new Promise<T[] | null>((resolve, reject) => {
      this.#waiter = { resolve, reject };
    });
    this.#updateRef();
    return p;
  }

  #claim(): void {
    if (this.#claimed) throw new OsmStreamError("An OsmStream can only be iterated once");
    this.#claimed = true;
  }

  /** Iterate batches of records (arrays of up to `batchSize`). */
  async *batches(): AsyncGenerator<T[], void, undefined> {
    this.#claim();
    try {
      for (;;) {
        const batch = await this.#next();
        if (batch === null) return;
        yield batch;
      }
    } finally {
      this.close();
    }
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<T, void, undefined> {
    for await (const batch of this.batches()) {
      yield* batch;
    }
  }

  /** Stop the export. Iteration ends; {@link stats} resolves with `stopped: true`. */
  close(): void {
    if (this.#closed) return;
    this.#closed = true;
    this.#queue.length = 0;
    this.#signal?.removeEventListener("abort", this.#abort);
    if (this.#native) {
      this.#native.close();
      // Stay alive until the end event arrives, so `stats` resolves.
      this.#updateRef();
    } else if (!this.#ended) {
      // Never started: nothing ran.
      this.#ended = true;
      this.#resolveStats(this.#emptyStats(true));
    }
    this.#wake();
  }

  /** Collect every record. Only for selections that fit in memory. */
  async toArray(): Promise<T[]> {
    const out: T[] = [];
    for await (const batch of this.batches()) {
      for (const r of batch) out.push(r);
    }
    return out;
  }

  /** Map records (lazily, keeping the stream's backpressure). */
  async *map<U>(fn: (record: T) => U | Promise<U>): AsyncGenerator<Awaited<U>, void, undefined> {
    for await (const r of this) yield await fn(r);
  }

  /** Keep records for which `fn` is true (lazily). */
  async *filter(fn: (record: T) => boolean | Promise<boolean>): AsyncGenerator<T, void, undefined> {
    for await (const r of this) if (await fn(r)) yield r;
  }

  /** An object-mode `Readable` (for `pipeline()`); backpressure is preserved. */
  toReadable(): Readable {
    return Readable.from(this, { objectMode: true });
  }
}

export function emptyStats(stopped: boolean): ExportStats {
  return {
    emitted: 0,
    points: 0,
    lines: 0,
    multiLines: 0,
    polygons: 0,
    waysMissingNodes: 0,
    relationsMissingWays: 0,
    invalidAreas: 0,
    storedNodes: 0,
    storedWays: 0,
    storeBytes: 0,
    stopped,
    elapsedMs: 0,
  };
}
