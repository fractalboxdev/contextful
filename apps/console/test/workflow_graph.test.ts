import test from "node:test";
import assert from "node:assert/strict";
import { mkdirSync, rmSync } from "node:fs";
import { createElement, type ComponentType } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { build } from "esbuild";
import { workflowGraph, type Workflows } from "../client/graph.ts";

/** The engine's published workflow for three pipelines, two models, a build and a
 * store-driven job, a publication and two external steps. */
const published: Workflows = {
  applied: 1,
  pipelines: [
    { id: "enrich", schedule: "every 1d", steps: [] },
    { id: "ingest", schedule: "every 1h", steps: ["select", "cast"] },
    { id: "normalize", after: "ingest", steps: [] },
  ],
  jobs: [
    { name: "score", kind: "store-driven" },
    { name: "summarize", kind: "build", after: "normalize", target: "daily_summary" },
  ],
  models: [{ id: "daily_summary", publish: false }, { id: "weekly_rollup", publish: false }],
  publications: [{ name: "scores-feed", job: "score", members: ["scores-copy"] }],
  externals: [
    { name: "review", label: "Analyst review", inputs: ["table:normalize_records"], outputs: ["pipeline:enrich"] },
    { name: "dashboard", label: "Team dashboard", inputs: ["table:weekly_rollup", "job:score"], outputs: [] },
  ],
  tables: ["enrich_records", "ingest_records", "normalize_records", "scores"],
  edges: [
    { from: "external:review", to: "pipeline:enrich", via: "output" },
    { from: "job:score", to: "external:dashboard", via: "input" },
    { from: "job:score", to: "publication:scores-feed", via: "publishes" },
    { from: "job:score", to: "table:scores", via: "writes" },
    { from: "job:summarize", to: "model:daily_summary", via: "builds" },
    { from: "model:daily_summary", to: "model:weekly_rollup", via: "reads" },
    { from: "model:weekly_rollup", to: "external:dashboard", via: "input" },
    { from: "pipeline:enrich", to: "table:enrich_records", via: "lands" },
    { from: "pipeline:ingest", to: "pipeline:normalize", via: "after" },
    { from: "pipeline:ingest", to: "table:ingest_records", via: "lands" },
    { from: "pipeline:normalize", to: "job:summarize", via: "after" },
    { from: "pipeline:normalize", to: "table:normalize_records", via: "lands" },
    { from: "table:enrich_records", to: "job:score", via: "reads" },
    { from: "table:normalize_records", to: "external:review", via: "input" },
    { from: "table:normalize_records", to: "model:daily_summary", via: "reads" },
  ],
  runs: { ingest: { status: "success", run_id: "run-1" }, "job:score": { status: "failed", run_id: "run-2" } },
};

async function renderGraph(workflows: Workflows): Promise<string> {
  return renderAdmin("WorkflowGraph", { workflows });
}

async function renderAdmin(component: string, props: object): Promise<string> {
  const dir = new URL("../client/dist/", import.meta.url);
  mkdirSync(dir, { recursive: true });
  const outfile = new URL(`.workflow-graph-${process.pid}.mjs`, dir);
  await build({
    entryPoints: [new URL("../client/admin.tsx", import.meta.url).pathname], bundle: true, write: true, jsx: "automatic",
    platform: "node", format: "esm", target: "node22", packages: "external", logLevel: "warning", outfile: outfile.pathname,
  });
  try {
    const components = await import(`${outfile.href}?component=${component}`) as Record<string, ComponentType<object>>;
    return renderToStaticMarkup(createElement(components[component], props));
  } finally {
    rmSync(outfile);
  }
}

test("source rows distinguish applied pending state, first failure and latest successful rows", async () => {
  const html = await renderAdmin("SourceList", { sources: [
    { id: "inbox", connector: "http", destinations: ["inbox_items"], version: 2, trigger: "manual", state: "pending", first_run: null, latest_run: null },
    { id: "guest", connector: "source.wasm", destinations: ["guest_items"], version: 3, trigger: "schedule", schedule: "every 1h", state: "success",
      first_run: { run_id: "first", status: "failed", rows: 0, message: "Renew the source credential." }, latest_run: { run_id: "latest", status: "success", rows: 3 } },
  ] });
  assert.match(html, /Applied version 2/);
  assert.match(html, /Waiting for the first run of this applied version/);
  assert.match(html, /Applying a source does not start a run/);
  assert.match(html, /First run first: failed, 0 rows/);
  assert.match(html, /Renew the source credential/);
  assert.match(html, /Latest run latest: success, 3 rows/);
  assert.match(html, /every 1h/);
  assert.match(html, /guest_items/);
});

test("source rows show a retried fire's changed failure reason under the same identity", async () => {
  const html = await renderAdmin("SourceList", { sources: [
    { id: "inbox", connector: "receive", destinations: ["inbox_items"], version: 3, trigger: "receive", state: "failed",
      first_run: { run_id: "receive-group", status: "failed", rows: 0, message: "Wait for the source rate limit." },
      latest_run: { run_id: "receive-group", status: "failed", rows: 0, message: "Renew the source credential." } },
  ] });
  assert.match(html, /First run: Wait for the source rate limit/);
  assert.match(html, /Latest run: Renew the source credential/);
});

function nodes(html: string): Map<string, string> {
  const found = new Map<string, string>();
  for (const part of html.split('<div class="node ').slice(1)) {
    found.set(/data-node="([^"]+)"/.exec(part)![1], part.split("<ul")[0]);
  }
  return found;
}

test("the layout places every published node by its longest upstream path", () => {
  const graph = workflowGraph(published);
  assert.equal(graph.nodes.length, 14);
  assert.equal(graph.edges.length, 15);
  const column = (id: string) => graph.nodes.find((n) => n.id === id)!.column;
  assert.equal(column("pipeline:ingest"), 0);
  assert.equal(column("pipeline:normalize"), 1);
  assert.equal(column("table:normalize_records"), 2);
  assert.equal(column("external:review"), 3);
  assert.equal(column("pipeline:enrich"), 4);
  assert.ok(column("external:dashboard") > column("model:weekly_rollup"));
  for (const edge of graph.edges) assert.ok(column(edge.from) < column(edge.to), `${edge.from} precedes ${edge.to}`);
  assert.deepEqual(workflowGraph({ ...published, edges: [...published.edges!, { from: "table:gone", to: "job:score", via: "reads" }] }).edges.length, 15);
});

// spec: surface.visualize.external-step@2e29d135
test("the canvas draws external steps as nodes marked outside the engine, with no run badge", async () => {
  const html = await renderGraph(published);
  assert.match(html, /data-graph-nodes="14"/);
  assert.match(html, /data-graph-edges="15"/);
  const drawn = nodes(html);
  assert.equal(drawn.size, 14);
  for (const name of ["review", "dashboard"]) {
    const external = drawn.get(`external:${name}`)!;
    assert.match(external, /data-engine="external"/);
    assert.match(external, /Runs outside the engine/);
    assert.doesNotMatch(external, /class="[^"]*\brun\b/);
    assert.doesNotMatch(external, /No runs recorded/);
  }
  assert.match(drawn.get("external:review")!, /Analyst review/);
  assert.match(drawn.get("pipeline:ingest")!, /data-engine="engine"/);
  assert.match(drawn.get("pipeline:ingest")!, /success/);
  assert.match(drawn.get("job:score")!, /failed/);
  const edges = [...html.matchAll(/<path class="edge" data-from="([^"]+)" data-to="([^"]+)" data-via="([^"]+)"/g)].map((m) => `${m[1]} ${m[3]} ${m[2]}`);
  assert.equal(edges.length, 15);
  assert.ok(edges.includes("table:normalize_records input external:review"));
  assert.ok(edges.includes("external:review output pipeline:enrich"));
  assert.match(html, /Analyst review output enrich/);
});

test("an empty published workflow draws no canvas", async () => {
  assert.match(await renderGraph({ applied: null, pipelines: [], edges: [] }), /No workflows are published/);
});
