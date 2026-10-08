import assert from "node:assert/strict";
import { test } from "node:test";
import { createConsole, issueCognitoSession } from "../src/index.ts";

function hosted() {
  const calls: string[] = [];
  const app = createConsole({
    identity: { kind: "cognito", sessionSecret: "session-secret", queryGroup: "query", adminGroup: "admin" },
    stores: [{ id: "field-notes", label: "Field notes" }],
    turn: async () => ({ answer: "", sources: [], widgets: [] }),
    read: { list: async () => [] },
    browse: {
      discover: async ({ operator, store, asOf }) => { calls.push(`discover:${operator.subject}:${store}:${asOf ?? ""}`); return {
        chips: [{ table: "filings", label: "Filings" }], insights: [{ table: "filings", label: "Filings", rows: 3 }], files: [{ table: "filings", path: "filings/runs/run-1/report.parquet", label: "Report" }],
      }; },
      preview: async ({ operator, store, path, asOf }) => { calls.push(`preview:${operator.subject}:${store}:${path}:${asOf ?? ""}`); return { columns: ["id"], rows: [["one"]] }; },
    },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  const session = issueCognitoSession({ subject: "reader", groups: ["query"], expiresAt: Math.floor(Date.now() / 1000) + 60 }, "session-secret");
  const request = (path: string, body?: unknown, origin = "https://console.example") => app.fetch(new Request(`https://console.example${path}`, {
    method: body === undefined ? "GET" : "POST", headers: { cookie: `console_session=${session}`, origin },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
  }));
  return { request, calls };
}

test("hosted Query exposes bounded same-origin browse and preview routes", async () => {
  const { request, calls } = hosted();
  const page = await (await request("/query")).text();
  for (const id of ["chips", "insights", "file-gallery", "file-preview"]) assert.match(page, new RegExp(`id="${id}"`));
  assert.match(page, /\/query\/api\/browse/);
  assert.match(page, /\/query\/api\/preview/);
  const asOf = "2030-01-01T00:00:00Z";
  const browse = await request("/query/api/browse", { store: "field-notes", asOf });
  assert.equal(browse.status, 200);
  assert.equal((await browse.json() as { chips: unknown[] }).chips.length, 1);
  const preview = await request("/query/api/preview", { store: "field-notes", path: "filings/runs/run-1/report.parquet", asOf });
  assert.equal(preview.status, 200);
  assert.deepEqual((await preview.json() as { rows: unknown[] }).rows, [["one"]]);
  assert.deepEqual(calls, [`discover:reader:field-notes:${asOf}`, `preview:reader:field-notes:filings/runs/run-1/report.parquet:${asOf}`]);
});

test("hosted browse rejects foreign origins, invalid bounds, and oversized paths before dispatch", async () => {
  const { request, calls } = hosted();
  assert.equal((await request("/query/api/browse", { store: "field-notes" }, "https://foreign.example")).status, 403);
  assert.equal((await request("/query/api/browse", { store: "field-notes", asOf: "not-a-date" })).status, 400);
  assert.equal((await request("/query/api/preview", { store: "field-notes", path: "x".repeat(4097) })).status, 400);
  assert.equal((await request("/query/api/browse", { store: "unknown" })).status, 400);
  assert.deepEqual(calls, []);
});
