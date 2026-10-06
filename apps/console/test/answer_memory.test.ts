import assert from "node:assert/strict";
import { test } from "node:test";
import { createLiveAnswer } from "../src/answer_live.ts";
import type { StoreEntry } from "../../gateway/src/index.ts";

test("verified reading session lands evidence-backed claims and recalls them before planning after restart", async () => {
  const store: StoreEntry = { id: "field-notes", label: "Field notes", endpoint: "https://store.example", auth: "exchange",
    exchangeRoute: "/auth/exchange", credentialName: "FIELD_NOTES_QUERY_TOKEN", bindingName: "FIELD_NOTES" };
  const events: string[] = [];
  const claims: Array<{ subject: string; predicate: string; object: string; scope: string }> = [];
  const writes: Array<{ authorization: string | null; body: Record<string, unknown> }> = [];
  const fetcher: typeof fetch = async (input, init) => {
    const url = String(input);
    if (url.endsWith("/auth/exchange")) return Response.json({ token: "reader-writer-alice" });
    if (url.endsWith("/memory/claims")) {
      events.push("write");
      const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
      writes.push({ authorization: new Headers(init?.headers).get("authorization"), body });
      const claim = body.claim as { subject: string; predicate: string; object: string };
      claims.push({ ...claim, scope: `console:${body.actor}:${body.session}` });
      return Response.json({ landed: true, scope: claims.at(-1)?.scope, claim_id: "claim-1" });
    }
    if (url.endsWith("/mcp")) {
      const message = JSON.parse(String(init?.body)) as { id: number; params: { name: string; arguments: Record<string, unknown> } };
      events.push(message.params.name);
      let result: unknown;
      if (message.params.name === "context.describe") result = { tables: [
        { table: "filings", kind: "data", description: "Northwind filings" },
        { table: "memory/facts", kind: "memory" },
      ] };
      else if (message.params.name === "memory.recall") result = { columns: ["subject", "predicate", "object", "scope"],
        rows: claims.filter((claim) => claim.subject === message.params.arguments.subject)
          .map((claim) => [claim.subject, claim.predicate, claim.object, claim.scope]) };
      else {
        assert.equal(message.params.name, "context.query");
        const sql = String(message.params.arguments.sql);
        result = sql.includes("DISTINCT subject") ? { columns: ["subject"], rows: [["Northwind"]] } : {
          columns: ["_run_id", "_row_seq", "filing_id", "title", "summary"],
          rows: [["run-1", 0, "filing-1", "Northwind filing", "Northwind filings need review"]],
        };
      }
      return Response.json({ jsonrpc: "2.0", id: message.id, result: { structuredContent: result } });
    }
    assert.equal(url, "https://model.example/v1/chat/completions");
    events.push("model");
    const prompt = String(init?.body);
    const content = prompt.includes("Distil") ? JSON.stringify({ entries: [
      { subject: "Northwind", key: "filings", learning: "Northwind filings need review" },
    ] }) : "Northwind filings need review [filing-1].";
    if (claims.length > 0 && !prompt.includes("Distil")) assert.match(prompt, /Northwind filings need review/);
    return Response.json({ choices: [{ message: { content } }] });
  };
  const options = { stores: [store], env: { CONTEXTFUL_MODEL_ENDPOINT: "https://model.example/v1", CONTEXTFUL_MODEL_ID: "fixture" }, fetcher };
  const operator = { subject: "alice", session: "reading-1", grants: new Set(["query" as const]), assertion: "verified-assertion" };
  const first = await createLiveAnswer(options).turn({ operator, store: store.id, question: "Which Northwind filing arrived?" });
  assert.match(first.answer, /\[source-1\]/);
  assert.equal(writes.length, 1);
  assert.equal(writes[0].authorization, "Bearer reader-writer-alice");
  assert.equal(writes[0].body.actor, "alice");
  assert.equal(writes[0].body.session, "reading-1");
  assert.equal(writes[0].body.into, "memory/facts");
  assert.equal(typeof writes[0].body.dedup_key, "string");
  assert.deepEqual((writes[0].body.claim as { evidence: unknown; scope?: string }).evidence,
    [{ table: "filings", run: "run-1", seq: 0 }]);
  assert.equal((writes[0].body.claim as { scope?: string }).scope, undefined);

  events.length = 0;
  await createLiveAnswer(options).turn({ operator, store: store.id, question: "What changed for Northwind?" });
  assert(events.indexOf("memory.recall") >= 0 && events.indexOf("memory.recall") < events.lastIndexOf("context.query"));
  assert(!events.includes("memory.write"), "read MCP remains closed to mutation");
});
