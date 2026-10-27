/** The results one measurement run holds, the issue table and the ledger records they render to. */
import { quantile, type HydrationLog, type Summary } from "../src/plan.ts";

export type FixtureFacts = { name: string; prefix: string; runs: number; rows: number; objects: number; parquet_bytes: number };
export type Hydration = { ready_ms: number; hydrated_ms: number; log: HydrationLog | null };
export type Lazy = { boot_ms: number; first_read_ms: number; warm_read_ms: number; parts: number };

export type Results = {
  target: string;
  runner: string;
  sleep_after_secs: number;
  fixtures: FixtureFacts[];
  ready: (Summary & { samples: number[] }) | null;
  hydrate: Array<{ fixture: string; runs: Hydration[] }>;
  lazy: Array<{ fixture: string; runs: Lazy[] }>;
  idle: { fixture: string; e2e_ms: number; attempts: number } | null;
  warm: { fixture: string; point: Summary; range: Summary; aggregate: Summary; join: Summary } | null;
};

/** The ledger entries a run fills (`evals/ledger.toml`, trend tier). */
export const LEDGER = {
  coldStart: "container-cold-start-wall",
  hydrate: "container-hydrate-wall",
  idleRead: "container-first-read-after-idle",
} as const;
/** The fixture `container-hydrate-wall` records. */
export const HYDRATE_LEDGER_FIXTURE = "1gb-8k";

export type LedgerRecord = { id: string; value: number; n: number; seed: number; run: { processor: string; nproc: number; memory_limit: null } };

const median = (xs: number[]) => quantile(xs, 0.5);
const ms = (x: number) => `${Math.round(x)} ms`;
const s = (x: number) => `${(x / 1000).toFixed(1)} s`;
const mb = (x: number) => `${(x / 1e6).toFixed(1)} MB`;

function fixtureCell(results: Results, name: string): string {
  const f = results.fixtures.find((x) => x.name === name);
  return f ? `\`${name}\`: ${f.rows.toLocaleString("en-US")} rows, ${f.objects.toLocaleString("en-US")} objects, ${mb(f.parquet_bytes)} Parquet` : `\`${name}\``;
}

/** The issue's measurement table, filled with what `results` holds. */
export function renderTable(results: Results): string {
  const rows: string[][] = [];
  if (results.ready) {
    const r = results.ready;
    rows.push(["Container start to `/health` ready", "Empty disk", `p50 ${ms(r.p50)}, p99 ${ms(r.p99)} over ${r.n} starts (min ${ms(r.min)}, max ${ms(r.max)})`]);
  }
  for (const h of results.hydrate) {
    const wall = median(h.runs.map((x) => x.hydrated_ms));
    const log = h.runs.find((x) => x.log)?.log;
    const pulled = log?.objects != null ? `; ${log.objects} objects and ${log.pointers} pointers pulled` : "";
    const mapped = log?.bytes != null ? `; ${log.files} files, ${mb(log.bytes)} mapped` : "";
    rows.push(["Start to `/health?ready=hydrated`", fixtureCell(results, h.fixture),
      `median ${s(wall)} over ${h.runs.length} (ready ${ms(median(h.runs.map((x) => x.ready_ms)))})${pulled}${mapped}`]);
  }
  for (const l of results.lazy) {
    const boot = median(l.runs.map((x) => x.boot_ms));
    const first = median(l.runs.map((x) => x.first_read_ms));
    const warm = median(l.runs.map((x) => x.warm_read_ms));
    const parts = l.runs[0]?.parts ?? 0;
    const perPart = parts > 0 ? `; ${ms(Math.max(0, first - warm) / parts)} per part over ${parts} parts on first use` : "";
    rows.push(["Same, with `[sync] lazy_parts`", fixtureCell(results, l.fixture),
      `time to first read ${s(boot + first)} (hydrated ${s(boot)}, first aggregate ${s(first)}, warm aggregate ${ms(warm)})${perPart}`]);
  }
  if (results.idle) {
    rows.push(["First read after `sleepAfter` elapses", fixtureCell(results, results.idle.fixture),
      `${s(results.idle.e2e_ms)} end to end ${results.target === "cloudflare" ? "through the routing Worker" : "on loopback"}, ${results.idle.attempts} request(s), sleepAfter ${results.sleep_after_secs} s`]);
  }
  if (results.warm) {
    const cell = fixtureCell(results, results.warm.fixture);
    const shapes: Array<[string, Summary]> = [
      ["Point lookup on the sorted `id`", results.warm.point],
      ["One-day range (`day =`)", results.warm.range],
      ["GROUP BY `k` over every row", results.warm.aggregate],
      ["Join to the 1,000-row `dims`, grouped by name", results.warm.join],
    ];
    for (const [shape, x] of shapes) rows.push(["Warm read", `${shape}, ${cell}`, `p50 ${ms(x.p50)}, p99 ${ms(x.p99)} over ${x.n}`]);
  }
  const header = "| Measurement | Fixture | Result |\n| --- | --- | --- |\n";
  return `Target: ${results.target} (${results.runner})\n\n` + header + rows.map((r) => `| ${r.join(" | ")} |`).join("\n") + "\n";
}

/** The ledger records `results` fills; an unmeasured entry has none. */
export function ledgerRecords(results: Results, seed: number): LedgerRecord[] {
  const run = { processor: results.runner, nproc: 0, memory_limit: null };
  const out: LedgerRecord[] = [];
  if (results.ready) out.push({ id: LEDGER.coldStart, value: results.ready.p50, n: results.ready.n, seed, run });
  const hydrate = results.hydrate.find((h) => h.fixture === HYDRATE_LEDGER_FIXTURE);
  if (hydrate && hydrate.runs.length) {
    out.push({ id: LEDGER.hydrate, value: median(hydrate.runs.map((x) => x.hydrated_ms)), n: hydrate.runs.length, seed, run });
  }
  if (results.idle) out.push({ id: LEDGER.idleRead, value: results.idle.e2e_ms, n: 1, seed, run });
  return out;
}
