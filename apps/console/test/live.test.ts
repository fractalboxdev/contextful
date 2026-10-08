import assert from "node:assert/strict";
import { test } from "node:test";
import { createLiveTurn } from "../src/live.ts";
import { ConsoleError } from "../src/turn.ts";
import { createConsole, issueCognitoSession } from "../src/index.ts";
import type { StoreEntry } from "../../gateway/src/index.ts";

test("Query redacts long denylisted values and credentials from prose and source URLs", async () => {
  const assertion = "assertion-" + "a".repeat(512);
  const credential = "credential-" + "c".repeat(512);
  const denied = "private-" + "p".repeat(256);
  const fetcher: typeof fetch = async (input, init) => {
    const url = String(input);
    if (url.endsWith("/auth/exchange")) return Response.json({ token: credential });
    if (url.endsWith("/mcp")) {
      const body = JSON.parse(String(init?.body));
      const value = body.params.name === "context.describe" ? { tables: [{ table: "filings", kind: "data" }] } : {
        columns: ["filing_id", "summary", "source_url"],
        rows: [["filing-1", "A filing arrived", `https://example.test/${credential}`]],
      };
      return Response.json({ result: { structuredContent: value } });
    }
    return Response.json({ choices: [{ message: { content: `A filing arrived [filing-1]. ${assertion} ${credential} ${denied}` } }] });
  };
  const turn = createLiveTurn({ stores: [store], env: {
    CONTEXTFUL_MODEL_ENDPOINT: "https://model.example/v1", CONTEXTFUL_MODEL_ID: "fixture",
    CONTEXTFUL_CONSOLE_DENYLIST: JSON.stringify([denied]),
  }, fetcher });
  const result = await turn({ operator: { subject: "reader", grants: new Set(["query"]), assertion },
    store: store.id, question: "Which filing arrived?" });
  const visible = JSON.stringify(result);
  for (const secret of [assertion, credential, denied]) assert.equal(visible.includes(secret), false);
  assert.match(result.answer, /A filing arrived \[source-1\]/);
  assert.equal((result.sources[0] as { url?: string }).url, undefined);
});

const store: StoreEntry = {
  id: "field-notes", label: "Field notes", endpoint: "https://store.example",
  auth: "exchange", exchangeRoute: "/auth/exchange", credentialName: "FIELD_NOTES_QUERY_TOKEN",
  bindingName: "FIELD_NOTES",
};

function fixture(options: { exchange?: boolean; exchangeStatus?: number; emptyToken?: boolean; shared?: string; rows?: unknown[][]; tables?: Array<{ table: string; kind: string }> } = {}) {
  const calls: Array<{ url: string; authorization: string | null; body: Record<string, unknown> }> = [];
  let modelCalls = 0;
  let modelBody: Record<string, unknown> | null = null;
  const fetcher: typeof fetch = async (input, init) => {
    const url = String(input);
    const body = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
    calls.push({ url, authorization: new Headers(init?.headers).get("authorization"), body });
    if (url.endsWith("/auth/exchange")) {
      const status = options.exchangeStatus ?? (options.exchange === false ? 403 : 200);
      return Response.json(status === 200 ? { token: options.emptyToken ? "" : "reader-token" } : { error: { identifier: status === 503 ? "ExchangeMaterialMissing" : "ExchangeAssertionInvalid" } }, { status });
    }
    if (url.endsWith("/mcp")) {
      const params = body.params as Record<string, unknown>;
      if (params.name === "context.describe") return Response.json({ jsonrpc: "2.0", id: body.id, result: { structuredContent: {
        tables: options.tables ?? [{ table: "filings", kind: "data" }],
      } } });
      assert.equal(params.name, "context.query");
      return Response.json({ jsonrpc: "2.0", id: body.id, result: { structuredContent: {
        columns: ["filing_id", "summary", "source_url"],
        rows: options.rows ?? [["filing-1", "Northwind filed on Monday", "https://example.test/filing-1"]],
      } } });
    }
    if (url.endsWith("/chat/completions")) {
      modelCalls++;
      modelBody = body;
      return Response.json({ choices: [{ message: { content: "Northwind filed on Monday [filing-1]." } }] });
    }
    throw new Error(`unexpected request ${url}`);
  };
  const turn = createLiveTurn({ stores: [store], env: {
    CONTEXTFUL_MODEL_ENDPOINT: "https://model.example/v1", CONTEXTFUL_MODEL_ID: "fixture",
    ...(options.shared ? { FIELD_NOTES_QUERY_TOKEN: options.shared } : {}),
  }, fetcher });
  return { turn, calls, modelCalls: () => modelCalls, modelBody: () => modelBody };
}

test("Query reads governed rows under the exchanged reader credential and cites their provenance", async () => {
  const { turn, calls, modelCalls } = fixture();
  const answer = await turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" });
  assert.match(answer.answer, /Northwind filed on Monday/);
  assert.match(JSON.stringify(answer.sources), /filing-1/);
  assert.equal(modelCalls(), 1);
  assert.equal(calls.find((call) => call.url.endsWith("/auth/exchange"))?.body.jwt, "verified-access-jwt");
  assert.equal(calls.find((call) => (call.body.params as Record<string, unknown> | undefined)?.name === "context.query")?.authorization, "Bearer reader-token");
  assert.deepEqual(calls.filter((call) => call.url.endsWith("/mcp")).map((call) => (call.body.params as Record<string, unknown>).name), ["context.describe", "context.query"]);
});

// spec: surface.ground.mint-refused@1ca62976
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

test("Query does not use a shared credential after exchange service failure", async () => {
  const { turn, calls } = fixture({ exchangeStatus: 503, shared: "shared-token" });
  await assert.rejects(turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" }));
  assert.equal(calls.filter((call) => call.url.endsWith("/mcp")).length, 0);
});

test("Query refuses an empty minted credential without using a shared credential", async () => {
  const { turn, calls } = fixture({ emptyToken: true, shared: "shared-token" });
  await assert.rejects(turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleTokenExchangeUnavailable");
  assert.equal(calls.filter((call) => call.url.endsWith("/mcp")).length, 0);
});

test("Query reads only a question-matched data table and refuses an unclear selection", async () => {
  const tables = [{ table: "filings", kind: "data" }, { table: "salaries", kind: "data" }];
  const matched = fixture({ tables });
  await matched.turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" });
  assert.deepEqual(matched.calls.filter((call) => (call.body.params as Record<string, unknown> | undefined)?.name === "context.query")
    .map((call) => (call.body.params as Record<string, unknown>).arguments),
    [{ sql: 'SELECT * FROM "filings"', limit: 8 }]);
  const unclear = fixture({ tables });
  await assert.rejects(unclear.turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "What changed?" }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleUngroundedAnswer");
  assert.equal(unclear.calls.filter((call) => call.url.endsWith("/mcp") && (call.body.params as Record<string, unknown>).name === "context.query").length, 0);
});

test("Query sends the model only rows represented by its eight visible sources", async () => {
  const rows = Array.from({ length: 9 }, (_, index) => [`filing-${index + 1}`, `Filing ${index + 1}`, `https://example.test/filing-${index + 1}`]);
  const { turn, modelBody } = fixture({ rows });
  const result = await turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" });
  assert.equal(result.sources.length, 8);
  assert.doesNotMatch(JSON.stringify(modelBody()), /filing-9/);
});

test("Query never sends rows without provenance to the model", async () => {
  const { turn, modelCalls } = fixture({ rows: [["", "Northwind filed on Monday", ""]] });
  await assert.rejects(turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" }),
    (error: unknown) => error instanceof ConsoleError && error.code === "ConsoleUngroundedAnswer");
  assert.equal(modelCalls(), 0);
});

test("Query excludes an unsourced sibling row from model context", async () => {
  const { turn, modelBody } = fixture({ rows: [
    ["filing-1", "Northwind filed on Monday", "https://example.test/filing-1"],
    ["", "PRIVATE UNSOURCED ROW", ""],
  ] });
  await turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" });
  assert.doesNotMatch(JSON.stringify(modelBody()), /PRIVATE UNSOURCED ROW/);
});

test("Query never scaffolds or dispatches a memory relation", async () => {
  const { turn, calls } = fixture({ tables: [{ table: "research/insights", kind: "memory" }, { table: "filings", kind: "data" }] });
  await turn({ operator: { subject: "operator-1", grants: new Set(["query"]), assertion: "verified-access-jwt" },
    store: "field-notes", question: "Which filing arrived?" });
  assert.deepEqual(calls.filter((call) => (call.body.params as Record<string, unknown> | undefined)?.name === "context.query")
    .map((call) => (call.body.params as Record<string, unknown>).arguments),
    [{ sql: 'SELECT * FROM "filings"', limit: 8 }]);
});

test("Query returns a typed exchange refusal through its hosted API", async () => {
  const { turn, calls } = fixture({ exchange: false });
  const app = createConsole({
    identity: { kind: "cognito", sessionSecret: "session-secret", queryGroup: "query", adminGroup: "admin" },
    stores: [{ id: store.id, label: store.label }], turn,
    read: { list: async () => [{ id: store.id, label: store.label }] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  const token = issueCognitoSession({ subject: "reader", groups: ["query"], assertion: "cognito-id-token",
    expiresAt: Math.floor(Date.now() / 1000) + 60 }, "session-secret");
  const response = await app.fetch(new Request("https://console.example/query/api/ask", {
    method: "POST", headers: { cookie: `console_session=${token}`, origin: "https://console.example" },
    body: JSON.stringify({ store: store.id, question: "Which filing arrived?" }),
  }));
  assert.equal(response.status, 403);
  assert.deepEqual(await response.json(), { error: { identifier: "ConsoleTokenExchangeRefused" } });
  assert.equal(calls.filter((call) => call.url.endsWith("/mcp")).length, 0);
});

test("Cognito Query exchanges the signed-in reader's assertion", async () => {
  const { turn, calls } = fixture();
  const app = createConsole({
    identity: { kind: "cognito", sessionSecret: "session-secret", queryGroup: "query", adminGroup: "admin" },
    stores: [{ id: store.id, label: store.label }], turn,
    read: { list: async () => [{ id: store.id, label: store.label }] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  const token = issueCognitoSession({ subject: "reader", groups: ["query"], assertion: "cognito-id-token",
    expiresAt: Math.floor(Date.now() / 1000) + 60 }, "session-secret");
  const response = await app.fetch(new Request("https://console.example/query/api/ask", {
    method: "POST", headers: { cookie: `console_session=${token}`, origin: "https://console.example" },
    body: JSON.stringify({ store: store.id, question: "Which filing arrived?" }),
  }));
  assert.equal(response.status, 200);
  assert.equal(calls.find((call) => call.url.endsWith("/auth/exchange"))?.body.jwt, "cognito-id-token");
});
