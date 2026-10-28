import assert from "node:assert/strict";
import { test } from "node:test";
import { dayOf, FIXTURES, healthHolds, QUERIES, OBJECTS_PER_PUSH, OBJECTS_PER_RUN, parseServeLog, planFixture, quantile, sameSecret, storeConfig, summarize } from "../src/plan.ts";

test("each fixture plan reaches its object count and Parquet bytes", () => {
  for (const fixture of FIXTURES) {
    const plan = planFixture(fixture, 1_000);
    const objects = plan.runs * OBJECTS_PER_RUN + OBJECTS_PER_PUSH;
    assert.ok(Math.abs(objects - fixture.objects) <= OBJECTS_PER_RUN, `${fixture.name}: ${objects} objects`);
    const bytes = plan.runs * plan.rowsPerRun * 1_000;
    assert.ok(bytes >= fixture.bytes && bytes < fixture.bytes * 1.05, `${fixture.name}: ${bytes} bytes`);
  }
});

test("a scaled plan keeps the object count and shrinks the bytes", () => {
  const full = planFixture(FIXTURES[3], 1_000);
  const scaled = planFixture(FIXTURES[3], 1_000, 0.01);
  assert.equal(scaled.runs, full.runs);
  assert.ok(scaled.rowsPerRun * 50 < full.rowsPerRun);
  assert.throws(() => planFixture(FIXTURES[0], 0), /FixturePlanInvalid/);
});

test("the store configuration pulls before serving and signs through env references", () => {
  const text = storeConfig("query", {
    endpoint: "r2://example", bucket: "corpus", prefix: "bench/1gb-8k", lazyParts: true,
    credentials: { accessKeyId: "env://A", secretAccessKey: "env://B" },
  });
  assert.equal(text, [
    "[node]", `id = "query"`, "", "[sync]", `endpoint = "r2://example"`, `bucket = "corpus"`, `prefix = "bench/1gb-8k"`,
    `coordination = "single-writer"`, "pull_before_run = true", "lazy_parts = true", `access_key_id = "env://A"`,
    `secret_access_key = "env://B"`, "",
  ].join("\n"));
  assert.equal(storeConfig("query", null), `[node]\nid = "query"\n`);
});

test("the serve log yields pulled objects and mapped bytes", () => {
  const log = "listening on http://0.0.0.0:8080/mcp\npull_before_run: pulled 7702 objects and 1 pointers\n" +
    "hydrated: snapshot set mapped (files: 3849, bytes: 1003401216)\n";
  assert.deepEqual(parseServeLog(log), { objects: 7702, pointers: 1, files: 3849, bytes: 1003401216 });
  assert.deepEqual(parseServeLog(""), { objects: null, pointers: null, files: null, bytes: null });
});

test("quantiles take the nearest rank", () => {
  const samples = Array.from({ length: 20 }, (_, i) => 20 - i);
  assert.equal(quantile(samples, 0.5), 10);
  assert.equal(quantile(samples, 0.99), 20);
  assert.deepEqual(summarize([3, 1, 2]), { n: 3, p50: 2, p99: 3, min: 1, max: 3 });
  assert.throws(() => quantile([], 0.5), /QuantileUndefined/);
});

test("the bench token compares whole and refuses an empty secret", () => {
  assert.equal(sameSecret("Bearer abc", "Bearer abc"), true);
  assert.equal(sameSecret("Bearer abd", "Bearer abc"), false);
  assert.equal(sameSecret("Bearer abc ", "Bearer abc"), false);
  assert.equal(sameSecret("", ""), false);
});

test("hydration holds only once the health body names it complete", () => {
  assert.equal(healthHolds("ready", 200, `{"hydration":"warming"}`), true);
  assert.equal(healthHolds("hydrated", 200, `{"hydration":"warming"}`), false);
  assert.equal(healthHolds("hydrated", 200, `{"hydration":"complete"}`), true);
  assert.equal(healthHolds("hydrated", 503, `{"hydration":"complete"}`), false);
  assert.equal(healthHolds("hydrated", 200, "not json"), false);
});

test("the range shape reads one day and refuses anything but a date", () => {
  assert.equal(dayOf(0), "2026-01-01");
  assert.equal(dayOf(50), "2026-02-20");
  assert.match(QUERIES.range(dayOf(50)), /WHERE day = '2026-02-20'$/);
  assert.throws(() => QUERIES.range("2026-01-01' OR 1=1 --"), /BenchDayInvalid/);
});
