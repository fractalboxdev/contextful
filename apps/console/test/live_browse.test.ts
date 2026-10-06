import assert from "node:assert/strict";
import { test } from "node:test";
import { createLiveBrowse } from "../src/live_browse.ts";
import type { StoreEntry } from "../../gateway/src/index.ts";

const store: StoreEntry = {
  id: "field-notes", label: "Field notes", endpoint: "https://store.example",
  auth: "exchange", exchangeRoute: "/auth/exchange", credentialName: "FIELD_NOTES_QUERY_TOKEN", bindingName: "FIELD_NOTES",
};

test("live browse uses one exchanged reader credential for governed discovery and preview", async () => {
  const calls: Array<{ name: string; args: Record<string, unknown>; authorization: string | null }> = [];
  let exchanges = 0;
  const fetcher: typeof fetch = async (input, init) => {
    const url = String(input);
    if (url.endsWith("/auth/exchange")) { exchanges++; return Response.json({ token: "reader-token" }); }
    assert.equal(url, "https://store.example/mcp");
    const body = JSON.parse(String(init?.body)) as { id: number; params: { name: string; arguments: Record<string, unknown> } };
    calls.push({ name: body.params.name, args: body.params.arguments, authorization: new Headers(init?.headers).get("authorization") });
    const value = body.params.name === "context.describe" && !body.params.arguments.table ? { tables: [
      { table: "filings", kind: "data", zone_admitted: true, description: "Filed reports" },
      { table: "payroll", kind: "data", zone_admitted: false },
      { table: "insights", kind: "memory", zone_admitted: true },
    ] } : body.params.name === "context.describe" ? { row_count: 3 } : body.params.name === "context.files" ? {
      columns: ["table", "path"], rows: [["filings", "filings/runs/run-1/report.parquet"], ["payroll", "payroll/runs/run-1/secret.parquet"]],
    } : body.params.name === "context.file" ? { columns: ["filing_id"], rows: [["filing-1"]] } : null;
    assert(value);
    return Response.json({ jsonrpc: "2.0", id: body.id, result: { structuredContent: value } });
  };
  const browse = createLiveBrowse({ stores: [store], env: {}, fetcher });
  const input = { operator: { subject: "reader", grants: new Set(["query" as const]), assertion: "verified-jwt" }, store: store.id,
    asOf: "2030-01-01T00:00:00Z" };
  const discovered = await browse.discover(input);
  assert.deepEqual(discovered.chips.map((chip: { table: string }) => chip.table), ["filings"]);
  assert.deepEqual(discovered.insights.map((insight: { rows: number }) => insight.rows), [3]);
  assert.deepEqual(discovered.files.map((file: { path: string }) => file.path), ["filings/runs/run-1/report.parquet"]);
  assert.equal(exchanges, 1);
  assert(calls.every((call) => call.authorization === "Bearer reader-token"));
  assert(calls.every((call) => call.args.as_of === input.asOf));
  const preview = await browse.preview({ ...input, path: "filings/runs/run-1/report.parquet" });
  assert.deepEqual(preview, { columns: ["filing_id"], rows: [["filing-1"]] });
  assert.equal(exchanges, 2);
  assert.equal(calls.at(-1)?.name, "context.file");
  const before = calls.length;
  await assert.rejects(browse.preview({ ...input, path: "filings/runs/run-1/other.parquet" }), /ConsoleGalleryPathUnlisted/);
  assert.equal(calls.filter((call) => call.name === "context.file").length, 1);
  assert(calls.length > before, "the refusal rechecks the governed file list");
});
