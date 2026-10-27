import assert from "node:assert/strict";
import { test } from "node:test";
import { ledgerRecords, renderTable, type Results } from "../bench/report.ts";
import { dimensionRows, rows, Stream } from "../bench/support.ts";

const results: Results = {
  target: "cloudflare",
  runner: "cloudflare container standard-1",
  sleep_after_secs: 300,
  fixtures: [{ name: "1gb-8k", prefix: "bench/1gb-8k", runs: 3998, rows: 1_000_000, objects: 8_000, landed_bytes: 1_000_000_000, parquet_bytes: 2_500_000_000 }],
  ready: { n: 2, p50: 1_200, p99: 2_400, min: 1_200, max: 2_400, samples: [1_200, 2_400] },
  hydrate: [{ fixture: "1gb-8k", runs: [
    { ready_ms: 1_500, hydrated_ms: 600_000, log: { objects: 8_000, pointers: 1, files: 3_998, bytes: 1_000_000_000 } },
    { ready_ms: 1_300, hydrated_ms: 500_000, log: null },
  ] }],
  lazy: [{ fixture: "1gb-8k", runs: [{ boot_ms: 4_000, first_read_ms: 44_000, warm_read_ms: 4_020, parts: 3998 }] }],
  idle: { fixture: "1gb-8k", e2e_ms: 610_000, attempts: 600 },
  warm: null,
  failures: [],
};

test("the table fills one row per measured section", () => {
  const table = renderTable(results);
  assert.match(table, /\| Container start to `\/health` ready \| Empty disk \| p50 1200 ms, p99 2400 ms over 2 starts/);
  assert.match(table, /median 550\.0 s over 2 \(600\.0 s, 500\.0 s; ready 1400 ms\); 8000 objects and 1 pointers pulled; 3998 files, 1000\.0 MB mapped/);
  assert.match(table, /time to first read 48\.0 s .* 10 ms per part over 3998 parts/);
  assert.match(table, /610\.0 s end to end through the routing Worker, 600 request\(s\), sleepAfter 300 s/);
  assert.doesNotMatch(table, /Warm read/);
});

test("the three trend entries take the ready p50, the 1gb-8k hydration median and the idle read", () => {
  assert.deepEqual(ledgerRecords(results, 7).map((r) => [r.id, r.value, r.n, r.seed]), [
    ["container-cold-start-wall", 1_200, 2, 7],
    ["container-hydrate-wall", 550_000, 2, 7],
    ["container-first-read-after-idle", 610_000, 1, 7],
  ]);
  assert.deepEqual(ledgerRecords({ ...results, ready: null, hydrate: [], idle: null }, 7), []);
});

test("fixture rows replay from their seed", () => {
  const a = rows(new Stream(1, "x"), 10, 3, "2026-01-01");
  assert.deepEqual(a, rows(new Stream(1, "x"), 10, 3, "2026-01-01"));
  assert.notDeepEqual(a, rows(new Stream(2, "x"), 10, 3, "2026-01-01"));
  assert.deepEqual(a.map((r) => [r.id, r.k]), [[10, 10], [11, 11], [12, 12]]);
  assert.equal(Buffer.from(a[0].pad, "base64").byteLength, 768);
});

test("every event key joins one of 1,000 dimension rows", () => {
  const dims = dimensionRows();
  assert.equal(new Set(dims.map((d) => d.k)).size, 1_000);
  assert.equal(new Set(dims.map((d) => d.name)).size, 37);
  assert.ok(rows(new Stream(1, "x"), 999, 2, "2026-01-01").every((r) => dims.some((d) => d.k === r.k)));
});
