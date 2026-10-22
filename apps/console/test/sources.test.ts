import test from "node:test";
import assert from "node:assert/strict";
import { appendSource, reduceDraft, sourceOutcome } from "../client/sources.ts";
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

test("a pending source declaration follows its form until the draft is replaced", () => {
  const applied = '[[pipeline]]\nid = "existing"\ntables = ["notes"]\n[pipeline.source]\nname = "http"\n';
  const declarations = (text: string) => text.match(/^\[\[pipeline\]\]$/gm)?.length ?? 0;
  let source = { id: "filings", connector: "absent-connector", table: "reports", schedule: "every 1h", config: 'endpoint = "https://example.test/feed"' };
  let draft = reduceDraft({ text: applied, pending: null }, { type: "add", source });
  assert.equal(declarations(draft.text), 2);
  assert.match(draft.text, /name = "absent-connector"/);
  // The store refuses the connector; the draft stays as sent, and correcting the field re-renders the one declaration.
  source = { ...source, connector: "http" };
  draft = reduceDraft(draft, { type: "field", source });
  assert.equal(draft.text, appendSource(applied, source));
  assert.equal(declarations(draft.text), 2);
  assert.doesNotMatch(draft.text, /absent-connector/);
  assert.equal(reduceDraft(draft, { type: "add", source }).text, draft.text, "adding again replaces the pending declaration");
  assert.equal(reduceDraft(draft, { type: "field", source: { ...source, id: " " } }).text, applied, "incomplete fields render the base document alone");
  // The corrected document validates and applies; the reloaded applied document carries no pending declaration.
  draft = reduceDraft(draft, { type: "replace", text: draft.text });
  assert.equal(draft.pending, null);
  assert.equal(reduceDraft(draft, { type: "field", source: { ...source, schedule: "" } }), draft, "a field change edits no applied document");
  assert.equal(declarations(reduceDraft(draft, { type: "add", source: { ...source, id: "second" } }).text), 3);
  // A raw edit takes the declaration over from the form.
  const raw = reduceDraft(reduceDraft({ text: applied, pending: null }, { type: "add", source }), { type: "replace", text: `${applied}# edited` });
  assert.equal(raw.pending, null);
  assert.equal(reduceDraft(raw, { type: "field", source }).text, `${applied}# edited`);
});

test("a newer run replicated from another node outranks an older local detail", () => {
  const pipeline = { id: "filings", tables: ["notes"] };
  const workflows: Workflows = { applied: 2, pipelines: [pipeline], source_runs: {},
    runs: { filings: { status: "failed", run_id: "newer-remote-run" } },
    run_details: { filings: { status: "success", rows: 9, run_id: "older-local-run" } } };
  const outcome = sourceOutcome(workflows, pipeline);
  assert.equal(outcome.latest, "failed · newer-remote-run");
  assert.equal(outcome.first, "Pending — no run confirmed for applied version 2");
  workflows.runs!.filings = { status: "success", run_id: "older-local-run" };
  assert.equal(sourceOutcome(workflows, pipeline).latest, "success · 9 rows · older-local-run");
  delete workflows.runs;
  assert.equal(sourceOutcome(workflows, pipeline).latest, "success · 9 rows · older-local-run");
  delete workflows.run_details;
  assert.equal(sourceOutcome(workflows, pipeline).latest, "No runs recorded");
});
