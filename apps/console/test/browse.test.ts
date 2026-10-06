import assert from "node:assert/strict";
import { test } from "node:test";
import { BrowseError, createBrowse, humanizeLabel } from "../src/browse.ts";

function fixture() {
  const calls: Array<{ tool: string; arguments: Record<string, unknown> }> = [];
  const transport = {
    async call(tool: string, args: Record<string, unknown>) {
      calls.push({ tool, arguments: args });
      if (tool === "context.describe" && !args.table) return { structuredContent: {
        tables: [
          { table: "research/vendor_filings", description: "Filed reports", zone_admitted: true },
          { table: "research/private_payroll", description: "Restricted", zone_admitted: false },
        ],
      } };
      if (tool === "context.describe") return { structuredContent: { table: args.table, row_count: 3 } };
      if (tool === "context.files") return { structuredContent: {
        columns: ["table", "path"],
        rows: [
          ["research/vendor_filings", "research/vendor_filings/runs/run-1/report.parquet"],
          ["research/private_payroll", "research/private_payroll/runs/run-1/secret.parquet"],
        ],
      } };
      if (tool === "context.file") return { structuredContent: { columns: ["title"], rows: [["Q1 report"]] } };
      throw new Error(`unexpected tool ${tool}`);
    },
  };
  return { browse: createBrowse(transport), calls };
}

test("browse advertises admitted tables as humanized chips", async () => {
  const { browse, calls } = fixture();
  const result = await browse.discover({ asOf: "2030-01-01T00:00:00Z" });
  assert.deepEqual(result.chips, [{ table: "research/vendor_filings", label: "Vendor Filings", description: "Filed reports" }]);
  assert.deepEqual(result.files, [{ table: "research/vendor_filings", path: "research/vendor_filings/runs/run-1/report.parquet", label: "Report" }]);
  assert.equal(calls[0].tool, "context.describe");
  assert.equal(calls[0].arguments.as_of, "2030-01-01T00:00:00Z");
  assert(calls.every((call) => call.tool.startsWith("context.")));
  assert(!calls.some((call) => call.arguments.table === "research/private_payroll"));
});

test("insights show governed table row counts", async () => {
  const { browse } = fixture();
  const result = await browse.discover({});
  assert.deepEqual(result.insights, [{ table: "research/vendor_filings", label: "Vendor Filings", rows: 3 }]);
});

test("a gallery preview refuses paths absent from the governed listing", async () => {
  const { browse, calls } = fixture();
  await assert.rejects(browse.preview({ path: "research/vendor_filings/runs/run-1/other.parquet" }),
    (error: unknown) => error instanceof BrowseError && error.code === "ConsoleGalleryPathUnlisted");
  assert(!calls.some((call) => call.tool === "context.file"));
});

test("a gallery preview uses context.file with the listing's snapshot bound", async () => {
  const { browse, calls } = fixture();
  const preview = await browse.preview({ path: "research/vendor_filings/runs/run-1/report.parquet", asOf: "2030-01-01T00:00:00Z" });
  assert.deepEqual(preview, { columns: ["title"], rows: [["Q1 report"]] });
  assert.deepEqual(calls.at(-1), { tool: "context.file", arguments: { path: "research/vendor_filings/runs/run-1/report.parquet", as_of: "2030-01-01T00:00:00Z" } });
});

test("in-band read refusals remain errors", async () => {
  const browse = createBrowse({ call: async () => ({ isError: true, structuredContent: { error: { identifier: "EnforceUnknownRelation" } } }) });
  await assert.rejects(browse.discover({}),
    (error: unknown) => error instanceof BrowseError && error.code === "EnforceUnknownRelation");
});

test("human labels handle separators and acronyms without adding prompts", () => {
  assert.equal(humanizeLabel("reports/q3_sec_filings"), "Q3 SEC Filings");
  assert.equal(humanizeLabel("customer-orders"), "Customer Orders");
});
