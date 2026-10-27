/**
 * Record the cold-start, hydration and read measurements against a target.
 *
 *   BENCH_TOKEN=… npm run measure -- --target cloudflare --url https://<bench-route> --sleep-after-secs 300
 *   npm run measure -- --target local --starts 5 --hydrate-runs 1 --warm 10       # loopback check
 *
 * `--warm-fixture` and `--idle-fixture` replace the fixtures the warm and idle sections use.
 *
 * `--sections ready,hydrate,lazy,warm,idle` selects measurements (default all). Results land in
 * `--out` as results.json, results.md and one ledger record per filled trend entry under
 * records/ (or $CONTEXTFUL_MEASURE_DIR when set).
 */
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { cpus } from "node:os";
import { join, resolve } from "node:path";
import { DAYS, dayOf, DIM_TABLE, FIXTURES, IDLE_FIXTURE, QUERIES, queryCall, summarize, TABLE, WARM_FIXTURE, ZONE } from "../src/plan.ts";
import { ledgerRecords, renderTable, type Hydration, type Lazy, type Results } from "./report.ts";
import { contextful, parseArgs, Stream, type FixtureManifest } from "./support.ts";
import { CloudflareTarget, LocalTarget, type Target } from "./targets.ts";

const args = parseArgs(process.argv.slice(2));
const bin = args.get("contextful") ?? "contextful";
const out = resolve(args.get("out") ?? "../../.tmp/cloudflare-query-bench");
const manifest = JSON.parse(readFileSync(join(out, "fixtures.json"), "utf8")) as FixtureManifest;
const sections = new Set((args.get("sections") ?? "ready,hydrate,lazy,warm,idle").split(","));
const starts = Number(args.get("starts") ?? "20");
const hydrateRuns = Number(args.get("hydrate-runs") ?? "3");
const warmReads = Number(args.get("warm") ?? "50");
const sleepAfterSecs = Number(args.get("sleep-after-secs") ?? "300");
const hydrateWaitMs = Number(args.get("hydrate-timeout-secs") ?? "3600") * 1000;
const kind = args.get("target") ?? "cloudflare";
const warmFixture = args.get("warm-fixture") ?? WARM_FIXTURE;
const idleFixture = args.get("idle-fixture") ?? IDLE_FIXTURE;

let target: Target;
let runner: string;
if (kind === "cloudflare") {
  const url = args.get("url") ?? process.env.BENCH_URL;
  const token = process.env.BENCH_TOKEN;
  if (!url || !token) throw new Error("BenchTargetUnbound: --url (or BENCH_URL) and BENCH_TOKEN are required for --target cloudflare");
  target = new CloudflareTarget(url, token);
  runner = `cloudflare container ${args.get("instance-type") ?? "standard-1"}`;
} else if (kind === "local") {
  if (!manifest.endpoint.startsWith("file://")) throw new Error("BenchTargetUnbound: --target local reads a file:// fixture bucket");
  target = new LocalTarget(bin, join(out, "local-serve"), manifest.endpoint, manifest.bucket, manifest.public_key);
  runner = `local ${cpus()[0]?.model ?? "cpu"} x${cpus().length}`;
} else {
  throw new Error(`BenchTargetUnknown: ${kind}`);
}

// A network checkpoint admits a bearer living at most 3600 s, so the driver re-mints before expiry.
let minted = { token: "", at: 0 };
const token = () => {
  if (Date.now() - minted.at > 50 * 60_000) {
    minted = {
      token: contextful(bin, manifest.issuer_dir, ["token", "mint", "--issuer-key", ".contextful/issuer.seed",
        "--on-behalf-of", "user://bench@example.test", "--zone", ZONE, "--table", TABLE, "--table", DIM_TABLE, "--ttl", "3600"]).trim(),
      at: Date.now(),
    };
  }
  return minted.token;
};
const fixture = (name: string) => {
  const f = manifest.fixtures.find((x) => x.name === name);
  if (!f) throw new Error(`BenchFixtureAbsent: ${name} is not in fixtures.json`);
  return f;
};
const built = FIXTURES.map((f) => f.name).filter((name) => manifest.fixtures.some((x) => x.name === name));

/** One read, timed by the caller; the aggregate's counts must cover every row. */
async function read(sql: string, expectRows?: number): Promise<number> {
  const at = performance.now();
  const answer = await target.query(queryCall(sql), token());
  const took = performance.now() - at;
  if (answer.status !== 200) throw new Error(`BenchReadFailed: ${answer.status} ${answer.body.slice(0, 300)}`);
  const result = JSON.parse(answer.body).result?.structuredContent as { rows?: unknown[][] } | undefined;
  if (!result?.rows) throw new Error(`BenchReadFailed: ${answer.body.slice(0, 300)}`);
  if (expectRows !== undefined) {
    const counted = result.rows.reduce((sum, row) => sum + Number(row[1]), 0);
    if (counted !== expectRows) throw new Error(`BenchReadIncomplete: the aggregate counted ${counted} of ${expectRows} rows`);
  }
  return took;
}

async function hydrated(prefix: string, lazy: boolean): Promise<Hydration> {
  await target.begin({ prefix, lazy_parts: lazy });
  const ready = await target.probe("ready", 60_000);
  if (!ready.held) throw new Error(`BenchNotReady: ${prefix} did not answer /health within 60 s`);
  const done = await target.probe("hydrated", hydrateWaitMs);
  if (!done.held) throw new Error(`BenchNotHydrated: ${prefix} did not hydrate within ${hydrateWaitMs / 1000} s`);
  return { ready_ms: ready.elapsed_ms, hydrated_ms: done.elapsed_ms, log: done.log };
}

const results: Results = {
  target: kind, runner, sleep_after_secs: sleepAfterSecs, fixtures: manifest.fixtures,
  ready: null, hydrate: [], lazy: [], idle: null, warm: null,
};
const log = (line: string) => console.error(`[${new Date().toISOString()}] ${line}`);

try {
  if (sections.has("ready")) {
    const samples: number[] = [];
    for (let i = 0; i < starts; i++) {
      await target.begin({ prefix: null, lazy_parts: false });
      const ready = await target.probe("ready", 60_000);
      if (!ready.held) throw new Error("BenchNotReady: an empty-disk container did not answer /health within 60 s");
      samples.push(ready.elapsed_ms);
      log(`ready ${i + 1}/${starts}: ${Math.round(ready.elapsed_ms)} ms`);
    }
    results.ready = { ...summarize(samples), samples };
  }
  if (sections.has("hydrate")) {
    for (const name of built) {
      const runs: Hydration[] = [];
      for (let i = 0; i < hydrateRuns; i++) {
        runs.push(await hydrated(fixture(name).prefix, false));
        log(`hydrate ${name} ${i + 1}/${hydrateRuns}: ${Math.round(runs[i].hydrated_ms)} ms ${JSON.stringify(runs[i].log)}`);
      }
      results.hydrate.push({ fixture: name, runs });
    }
  }
  if (sections.has("lazy")) {
    for (const name of built) {
      const f = fixture(name);
      const runs: Lazy[] = [];
      for (let i = 0; i < hydrateRuns; i++) {
        const boot = await hydrated(f.prefix, true);
        const first = await read(QUERIES.aggregate, f.rows);
        const warm: number[] = [];
        for (let j = 0; j < 5; j++) warm.push(await read(QUERIES.aggregate, f.rows));
        runs.push({ boot_ms: boot.hydrated_ms, first_read_ms: first, warm_read_ms: summarize(warm).p50, parts: results.hydrate.find((h) => h.fixture === name)?.runs[0]?.log?.files ?? f.runs });
        log(`lazy ${name} ${i + 1}/${hydrateRuns}: ${JSON.stringify(runs[i])}`);
      }
      results.lazy.push({ fixture: name, runs });
    }
  }
  if (sections.has("warm") && built.includes(warmFixture)) {
    const f = fixture(warmFixture);
    await hydrated(f.prefix, false);
    await read(QUERIES.aggregate, f.rows);
    const ids = new Stream(manifest.seed, "point");
    const days = new Stream(manifest.seed, "range");
    const shapes = { point: [] as number[], range: [] as number[], aggregate: [] as number[], join: [] as number[] };
    for (let i = 0; i < warmReads; i++) {
      shapes.point.push(await read(QUERIES.point(ids.below(f.rows))));
      shapes.range.push(await read(QUERIES.range(dayOf(days.below(DAYS)))));
      shapes.aggregate.push(await read(QUERIES.aggregate, f.rows));
      shapes.join.push(await read(QUERIES.join, f.rows));
    }
    results.warm = {
      fixture: warmFixture, point: summarize(shapes.point), range: summarize(shapes.range),
      aggregate: summarize(shapes.aggregate), join: summarize(shapes.join),
    };
    log(`warm ${JSON.stringify(results.warm)}`);
  }
  if (sections.has("idle") && built.includes(idleFixture)) {
    const f = fixture(idleFixture);
    await hydrated(f.prefix, false);
    await read(QUERIES.aggregate, f.rows);
    log(`idle: waiting for the container to sleep (sleepAfter ${sleepAfterSecs} s)`);
    await target.idle(sleepAfterSecs);
    const at = performance.now();
    let attempts = 0;
    for (;;) {
      attempts++;
      const answer = await target.query(queryCall(QUERIES.aggregate), token());
      if (answer.status === 200) break;
      if (answer.status !== 503) throw new Error(`BenchReadFailed: ${answer.status} ${answer.body.slice(0, 300)}`);
      if (performance.now() - at > hydrateWaitMs) throw new Error("BenchNotHydrated: the idle container never answered");
      await new Promise((r) => setTimeout(r, Math.min(Number(answer.retry_after ?? "1"), 1) * 1000));
    }
    results.idle = { fixture: idleFixture, e2e_ms: performance.now() - at, attempts };
    log(`idle ${JSON.stringify(results.idle)}`);
  }
} finally {
  await target.close();
}

mkdirSync(out, { recursive: true });
writeFileSync(join(out, "results.json"), JSON.stringify(results, null, 2) + "\n");
const table = renderTable(results);
writeFileSync(join(out, "results.md"), table);
const records = process.env.CONTEXTFUL_MEASURE_DIR ?? join(out, "records");
mkdirSync(records, { recursive: true });
for (const record of ledgerRecords(results, manifest.seed)) {
  writeFileSync(join(records, `${record.id}.json`), JSON.stringify(record, null, 2) + "\n");
}
console.log(table);
