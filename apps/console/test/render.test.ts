import test from "node:test";
import assert from "node:assert/strict";
import { buildView, readView, splitResult } from "../src/render.ts";

test("server result shape chooses metric, line, or table with a bar option", () => {
  assert.equal(buildView({ columns: ["revenue"], rows: [[42]] }).component, "metric.v1");
  assert.equal(buildView({ columns: ["day", "revenue"], rows: [["2026-01-01", 1], ["2026-01-02", 2], ["2026-01-03", 3]] }).component, "line.v1");
  assert.deepEqual(buildView({ columns: ["day", "revenue"], rows: [["2026-01-01", 1], ["2026-01-01", 2], ["2026-01-03", 3]] }).alternates, ["bar.v1"]);
  assert.equal(buildView({ columns: ["name", "value"], rows: [["A", 1]] }).component, "table.v1");
});

test("client and model view specifications never become widgets", () => {
  const result = { columns: ["amount"], rows: [[7]] };
  for (const origin of ["client", "model"] as const) {
    assert.throws(() => buildView(result, { origin, view: { component: "metric.v1" } }), /ConsoleViewNotServerBuilt/);
  }
  assert.equal(buildView(result, { origin: "server", hint: { component: "bar.v1", columns: ["missing"] } }).component, "metric.v1");
});

test("older transcripts render an unknown component as a table", () => {
  assert.equal(readView({ component: "map.v3", props: { columns: ["x"], rows: [[1]] } }).component, "table.v1");
});

test("tool return channels keep trace out of model grounding and view rows", () => {
  const channels = splitResult({ rows: [[1]], columns: ["count"], sources: [{ id: "s" }], trace: { query: "select secret" } });
  assert.deepEqual(channels.grounding, { rows: [[1]], columns: ["count"], sources: [{ id: "s" }] });
  assert.deepEqual(channels.trace, { query: "select secret" });
  assert.equal(channels.view.component, "metric.v1");
});
