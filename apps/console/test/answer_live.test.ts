import assert from "node:assert/strict";
import { test } from "node:test";
import { createLiveAnswer } from "../src/answer_live.ts";
import { deriveBrief } from "../src/brief.ts";
import { createConsole, issueCognitoSession } from "../src/index.ts";
import type { StoreEntry } from "../../gateway/src/index.ts";

const now = Date.parse("2030-01-08T12:00:00Z");
const store: StoreEntry = { id: "field-notes", label: "Field notes", endpoint: "https://store.example", auth: "exchange",
  exchangeRoute: "/auth/exchange", credentialName: "FIELD_NOTES_QUERY_TOKEN", bindingName: "FIELD_NOTES" };

function fixture() {
  const calls: Array<{ name: string; authorization: string | null }> = [];
  const modelPrompts: string[] = [];
  const fetcher: typeof fetch = async (input, init) => {
    const url = String(input);
    if (url.endsWith("/auth/exchange")) {
      const assertion = (JSON.parse(String(init?.body)) as { jwt: string }).jwt;
      return Response.json({ token: `reader-${assertion}` });
    }
    if (url.endsWith("/mcp")) {
      const body = JSON.parse(String(init?.body)) as { id: number; params: { name: string } };
      calls.push({ name: body.params.name, authorization: new Headers(init?.headers).get("authorization") });
      const value = body.params.name === "context.describe" ? { tables: [
        { table: "filings", kind: "data", description: "Northwind filings", zone_admitted: true },
      ] } : { columns: ["filing_id", "title", "summary", "published_at", "source_url", "owner_email"], rows: [
        ["filing-1", "Northwind filing", "Northwind filings need review", "2030-01-08", "https://example.test/filing-1", "secret@example.test"],
      ] };
      return Response.json({ jsonrpc: "2.0", id: body.id, result: { structuredContent: value } });
    }
    assert.equal(url, "https://model.example/v1/chat/completions");
    const prompt = String(init?.body);
    modelPrompts.push(prompt);
    const content = prompt.includes("Distil") ? JSON.stringify({ entries: [
      { subject: "Northwind", key: "filings", learning: "Northwind filings need review" },
      { subject: "Other", key: "invented", learning: "No evidence" },
    ] }) : "Northwind filings need review [filing-1].";
    return Response.json({ choices: [{ message: { content } }] });
  };
  const live = createLiveAnswer({ stores: [store], env: { CONTEXTFUL_MODEL_ENDPOINT: "https://model.example/v1",
    CONTEXTFUL_MODEL_ID: "fixture", CONTEXTFUL_CONSOLE_DENYLIST: '["secret@example.test"]' }, fetcher, clock: () => now });
  return { live, calls, modelPrompts };
}

test("live answer renders redacted governed rows and distils only observed scoped learning", async () => {
  const { live, calls, modelPrompts } = fixture();
  const alice = { subject: "alice", grants: new Set(["query" as const]), assertion: "alice-session-1" };
  const answer = await live.turn({ operator: alice, store: store.id, question: "Which Northwind filing arrived?" });
  assert.match(answer.answer, /Northwind filings need review/);
  assert.deepEqual(answer.resultRows?.columns, ["filing_id", "title", "summary", "published_at", "source_url", "owner_email"]);
  assert.doesNotMatch(JSON.stringify(answer.resultRows), /secret@example.test/);
  assert.equal(modelPrompts.length, 2, "the second model pass distils the completed answer");
  assert.doesNotMatch(modelPrompts[0], /secret@example.test/);
  assert(calls.every((call) => call.authorization === "Bearer reader-alice-session-1"));

  const sameSession = await live.brief(alice, store.id);
  assert.equal(sameSession.session.turns, 1);
  assert.equal(await deriveBrief({ ...sameSession, budgetMs: 100 }), null);
  const nextSession = await live.brief({ ...alice, assertion: "alice-session-2" }, store.id);
  assert.equal(nextSession.session.turns, 0);
  assert.deepEqual(nextSession.conclusions.map((entry) => entry.subject), ["Northwind"]);
  const card = await deriveBrief({ ...nextSession, budgetMs: 100 });
  assert.equal(card?.subjects[0].subject, "Northwind");
  assert(calls.some((call) => call.name === "context.query" && call.authorization === "Bearer reader-alice-session-2"));
  const bob = await live.brief({ subject: "bob", grants: new Set(["query"]), assertion: "bob-session" }, store.id);
  assert.deepEqual(bob.conclusions, []);
});

test("hosted Query publishes only to the verified operator and sanitizes its server-built widget", async () => {
  const { live } = fixture();
  const app = createConsole({
    identity: { kind: "cognito", sessionSecret: "session-secret", queryGroup: "query", adminGroup: "admin" },
    stores: [{ id: store.id, label: store.label }], turn: live.turn, brief: live.brief,
    briefBudgetMs: live.briefBudgetMs, redactView: live.redactView,
    read: { list: async () => [{ id: store.id, label: store.label }] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  const cookie = issueCognitoSession({ subject: "alice", groups: ["query"], assertion: "alice-session-1",
    expiresAt: Math.floor(Date.now() / 1000) + 60 }, "session-secret");
  const response = await app.fetch(new Request("https://console.example/query/api/ask", { method: "POST",
    headers: { cookie: `console_session=${cookie}`, origin: "https://console.example" },
    body: JSON.stringify({ store: store.id, question: "Which Northwind filing arrived?" }),
  }));
  assert.equal(response.status, 200);
  const body = await response.json() as { answer: string; widgets: Array<{ component: string; props: unknown }>; share?: boolean };
  assert.equal(body.share, undefined);
  assert.equal(body.widgets.length, 1);
  assert.equal(body.widgets[0].component, "table.v1");
  assert.doesNotMatch(JSON.stringify(body.widgets), /secret@example.test/);
});
