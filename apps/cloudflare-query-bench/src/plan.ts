/** Fixture shapes, store configuration and the figures a measurement reports. */

/** The project, table and audience every fixture store and query container share. */
export const PROJECT = "bench";
export const TABLE = "events";
export const AUDIENCE = "contextful://bench";
export const ZONE = "on-prem:hq";
/** The header carrying the contextful credential the bench Worker forwards as the MCP bearer. */
export const QUERY_TOKEN_HEADER = "Contextful-Authorization";
/** The port `contextful serve --http` listens on inside the query container. */
export const SERVE_PORT = 8080;
/** The seed every fixture row derives from. */
export const SEED = 0x5eed_0733;

/** Bucket objects one landed run adds: its Parquet part and its run manifest. */
export const OBJECTS_PER_RUN = 2;
/** Bucket objects a single push adds besides runs: run state, schema, bucket manifest, generation manifest. */
export const OBJECTS_PER_PUSH = 4;

export type Fixture = { name: string; bytes: number; objects: number };

const MB = 1_000_000;

/** The four hydration stores: 100 MB and 1 GB, each at about 1k and 8k bucket objects. */
export const FIXTURES: readonly Fixture[] = [
  { name: "100mb-1k", bytes: 100 * MB, objects: 1_000 },
  { name: "100mb-8k", bytes: 100 * MB, objects: 8_000 },
  { name: "1gb-1k", bytes: 1_000 * MB, objects: 1_000 },
  { name: "1gb-8k", bytes: 1_000 * MB, objects: 8_000 },
];

/** The fixture the idle-read and warm-read measurements run on. */
export const IDLE_FIXTURE = "1gb-8k";
export const WARM_FIXTURE = "1gb-1k";

export type Plan = { runs: number; rowsPerRun: number };

/**
 * Runs and rows per run reaching `fixture`'s object count and Parquet bytes, given the
 * calibrated on-disk bytes of one row. `scale` shrinks bytes alone, so a scaled fixture
 * keeps its object count.
 */
export function planFixture(fixture: Fixture, bytesPerRow: number, scale = 1): Plan {
  if (!(bytesPerRow > 0) || !(scale > 0)) throw new Error("FixturePlanInvalid");
  const runs = Math.max(1, Math.round((fixture.objects - OBJECTS_PER_PUSH) / OBJECTS_PER_RUN));
  const rows = (fixture.bytes * scale) / bytesPerRow;
  return { runs, rowsPerRun: Math.max(1, Math.ceil(rows / runs)) };
}

export type SyncTarget = {
  endpoint: string;
  bucket: string;
  prefix: string;
  lazyParts: boolean;
  /** Credentials as `env://NAME` references; absent for a `file://` endpoint. */
  credentials?: { accessKeyId: string; secretAccessKey: string };
};

const quoted = (value: string) => JSON.stringify(value);

/** `.contextful/context/<project>/config.toml` for node `node`, pulling `sync` before it serves. */
export function storeConfig(node: string, sync: SyncTarget | null, pullBeforeRun = true): string {
  const lines = ["[node]", `id = ${quoted(node)}`];
  if (sync) {
    lines.push("", "[sync]", `endpoint = ${quoted(sync.endpoint)}`, `bucket = ${quoted(sync.bucket)}`,
      `prefix = ${quoted(sync.prefix)}`, `coordination = "single-writer"`);
    if (pullBeforeRun) lines.push("pull_before_run = true");
    if (sync.lazyParts) lines.push("lazy_parts = true");
    if (sync.credentials) {
      lines.push(`access_key_id = ${quoted(sync.credentials.accessKeyId)}`,
        `secret_access_key = ${quoted(sync.credentials.secretAccessKey)}`);
    }
  }
  return lines.join("\n") + "\n";
}

/** The project's `contextful.toml`: one undeclared-key table. */
export const PROJECT_DECLARATION = `authoring_posture = "per_request"\n[[pipeline.tables]]\nname = ${quoted(TABLE)}\n`;

/** The SQL each read measurement sends through `context.query`. */
export const QUERIES = {
  point: (id: number) => `SELECT id, k, v FROM ${TABLE} WHERE id = ${Math.trunc(id)}`,
  aggregate: `SELECT k, count(*) AS n, avg(v) AS mean FROM ${TABLE} GROUP BY k ORDER BY k`,
} as const;

/** One JSON-RPC `tools/call` of `context.query`. */
export function queryCall(sql: string, id = 1): string {
  return JSON.stringify({ jsonrpc: "2.0", id, method: "tools/call", params: { name: "context.query", arguments: { sql } } });
}

/** What `serve` reports on stderr once its hydration pull and mapping finish. */
export type HydrationLog = { objects: number | null; pointers: number | null; files: number | null; bytes: number | null };

/** Parse the `pull_before_run: pulled …` and `hydrated: …` lines of a serve log. */
export function parseServeLog(log: string): HydrationLog {
  const pulled = /pulled (\d+) objects and (\d+) pointers/.exec(log);
  const mapped = /hydrated: snapshot set mapped \(files: (\d+), bytes: (\d+)\)/.exec(log);
  return {
    objects: pulled ? Number(pulled[1]) : null,
    pointers: pulled ? Number(pulled[2]) : null,
    files: mapped ? Number(mapped[1]) : null,
    bytes: mapped ? Number(mapped[2]) : null,
  };
}

/** The `q`-quantile of `samples` by the nearest-rank method. */
export function quantile(samples: readonly number[], q: number): number {
  if (samples.length === 0 || !(q > 0 && q <= 1)) throw new Error("QuantileUndefined");
  const sorted = [...samples].sort((a, b) => a - b);
  return sorted[Math.ceil(q * sorted.length) - 1];
}

export type Summary = { n: number; p50: number; p99: number; min: number; max: number };

export function summarize(samples: readonly number[]): Summary {
  return {
    n: samples.length,
    p50: quantile(samples, 0.5),
    p99: quantile(samples, 0.99),
    min: Math.min(...samples),
    max: Math.max(...samples),
  };
}

/** Constant-time equality of two header values. */
export function sameSecret(supplied: string, expected: string): boolean {
  const a = new TextEncoder().encode(supplied);
  const b = new TextEncoder().encode(expected);
  let diff = a.byteLength ^ b.byteLength;
  for (let i = 0; i < b.byteLength; i++) diff |= (a[i] ?? 0) ^ b[i];
  return expected.length > 0 && diff === 0;
}
