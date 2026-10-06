import assert from "node:assert/strict";
import { test } from "node:test";
import { createLiveTurn } from "../src/live.ts";
import type { StoreEntry } from "../../gateway/src/index.ts";

const store: StoreEntry = {
  id: "field-notes", label: "Field notes", endpoint: "https://store.example",
  auth: "exchange", exchangeRoute: "/auth/exchange", credentialName: "FIELD_NOTES_QUERY_TOKEN",
  bindingName: "FIELD_NOTES",
};

function fixture(options: { exchange?: boolean; shared?: string; rows?: unknown[][] } = {}) {
  const calls: Array<{ url: string; authorization: string | null; body: Record<string, unknown> }> = [];
  let modelCalls = 0;
  const fetcher: typeof fetch = async (input, init) => {
    const url = String(input);
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
    calls.push({ url, authorization: new Headers(init?.headers).get("authorization"), body });
    if (url.endsWith("/auth/exchange")) return Response.json(options.exchange === false ? { error: { identifier: "ExchangeAssertionInvalid" } } : { token: "reader-token" }, { status: options.exchange === false ? 403 : 200 });
    if (url.endsWith("/mcp")) {
      const params = body.params as Record<string, unknown>;
      if (params.name === "context.describe") return Response.json({ jsonrpc: "2.0", id: body.id, result: { structuredContent: { tables: [{ table: "filings" }] } } });
      assert.equal(params.name, "context.query");
      return Response.json({ jsonrpc: "2.0", id: body.id, result: { structuredContent: {
        columns: ["filing_id", "summary", "source_url"],
        rows: options.rows ?? [["filing-1", "Northwind filed on Monday", "https://example.test/filing-1"]],
      } } });
    }
    if (url.endsWith("/chat/completions")) {
      modelCalls++;
      return Response.json({ choices: [{ message: { content: "Northwind filed on Monday [filing-1]." } }] });
    }
    throw new Error(`unexpected request ${url}`);
  };
  const turn = createLiveTurn({ stores: [store], env: {
    CONTEXTFUL_MODEL_ENDPOINT: "https://model.example/v1", CONTEXTFUL_MODEL_ID: "fixture",
    ...(options.shared ? { FIELD_NOTES_QUERY_TOKEN: options.shared } : {}),
  }, fetcher });
  return { turn, calls, modelCalls: () => modelCalls };
}

test("Query reads governed rows under the exchanged reader credential and cites their provenance", async () => {
  const { turn, calls, modelCalls } = fixture();
  const answer = await turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" });
  assert.match(answer.answer, /Northwind filed on Monday/);
  assert.match(JSON.stringify(answer.sources), /filing-1/);
  assert.equal(modelCalls(), 1);
  assert.equal(calls.find((call) => call.url.endsWith("/auth/exchange"))?.body.assertion, "verified-access-jwt");
  assert.equal(calls.find((call) => (call.body.params as Record<string, unknown> | undefined)?.name === "context.query")?.authorization, "Bearer reader-token");
  assert.deepEqual(calls.filter((call) => call.url.endsWith("/mcp")).map((call) => (call.body.params as Record<string, unknown>).name), ["context.describe", "context.query"]);
});

test("Query falls back to a configured shared credential only after exchange refusal", async () => {
  const fallback = fixture({ exchange: false, shared: "shared-token" });
  await fallback.turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" });
  assert.equal(fallback.calls.find((call) => (call.body.params as Record<string, unknown> | undefined)?.name === "context.query")?.authorization, "Bearer shared-token");
  const refused = fixture({ exchange: false });
  await assert.rejects(refused.turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" }), /ConsoleTokenExchangeRefused/);
  assert.equal(refused.modelCalls(), 0);
  assert.equal(refused.calls.filter((call) => call.url.endsWith("/mcp")).length, 0);
});

test("Query never sends rows without provenance to the model", async () => {
  const { turn, modelCalls } = fixture({ rows: [["", "Northwind filed on Monday", ""]] });
  await assert.rejects(turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" }), /ConsoleUngroundedAnswer/);
  assert.equal(modelCalls(), 0);
});
