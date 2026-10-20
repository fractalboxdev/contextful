import test from "node:test";
import assert from "node:assert/strict";
import { appendSource, sourceOutcome } from "../client/sources.ts";
import type { Workflows } from "../client/graph.ts";

test("a source form appends a native or component declaration to the applied document", () => {
  const applied = '[[pipeline]]\nid = "existing"\ntables = ["notes"]\n[pipeline.source]\nname = "http"\n';
  for (const connector of ["http", "registry.example/filings@1"]) {
    const draft = appendSource(applied, { id: "filings", connector, table: "reports", schedule: "every 1h", config: 'endpoint = "https://example.test/feed"' });
    assert.ok(draft.startsWith(applied.trimEnd()));
    assert.match(draft, /id = "filings"\ntables = \["reports"\]\nschedule = "every 1h"/);
    assert.ok(draft.includes(`name = ${JSON.stringify(connector)}`));
    assert.match(draft, /\[pipeline.source.config\]\nendpoint = "https:\/\/example.test\/feed"/);
  }
  const triggered = appendSource(applied, { id: "triggered", connector: "http", table: "notes", schedule: "", config: "" });
  assert.doesNotMatch(triggered, /schedule =/);
  assert.throws(() => appendSource(applied, { id: "", connector: "http", table: "notes", schedule: "", config: "" }), /identity/);
});

test("first-run confirmation requires the applied version receipt and preserves zero-row success", () => {
  const pipeline = { id: "filings", source: "http" };
  const workflows: Workflows = { applied: 3, pipelines: [pipeline], run_details: { filings: { status: "success", rows: 9, run_id: "late-old-version" } },
    source_runs: { filings: { applied: 2, first: { status: "success", rows: 9 }, latest: { status: "success", rows: 9 } } } };
  assert.equal(sourceOutcome(workflows, pipeline).first, "Pending — no run confirmed for applied version 3");
  assert.match(sourceOutcome(workflows, pipeline).latest, /9 rows.*late-old-version/);
  workflows.source_runs!.filings = { applied: 3, first: { status: "success", rows: 0, run_id: "first-current" }, latest: { status: "failed", rows: 0, failure: "Permanent" } };
  assert.equal(sourceOutcome(workflows, pipeline).first, "success · 0 rows · first-current");
  workflows.source_runs!.filings.first = { status: "failed", rows: 0, failure: "Permanent" };
  assert.equal(sourceOutcome(workflows, pipeline).first, "failed · 0 rows · Check the source endpoint and request settings before retrying.");
  const multiple = { ...pipeline, tables: ["filings_a", "filings_b"] };
  workflows.source_runs!.filings.first = { status: "success", table: "filings_a", rows: 1 };
  assert.equal(sourceOutcome(workflows, multiple).first, "Table filings_a: success · 1 rows · Other output tables are pending confirmation.");
});
