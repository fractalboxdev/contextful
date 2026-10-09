import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createConsole, issueCognitoSession } from "../src/index.ts";
import { restoreSaved, saveSaved } from "../client/transcripts.ts";

const secret = "transcript-scope-fixture";
const stores = [{ id: "field-notes", label: "Field notes" }];

function app() {
  return createConsole({
    identity: { kind: "cognito", sessionSecret: secret, queryGroup: "query", adminGroup: "admin" },
    stores,
    turn: async () => ({ answer: "", sources: [], widgets: [] }),
    read: { list: async () => stores },
    control: { workflows: async () => [], record: async () => [], edit: async () => ({}), apply: async () => ({}) },
  });
}

async function scopeFor(subject: string, session?: string): Promise<string | null> {
  const cookie = issueCognitoSession({ subject, groups: ["query"], expiresAt: Math.floor(Date.now() / 1000) + 300, session }, secret);
  const response = await app().fetch(new Request("https://console.example/query/api/stores", { headers: { cookie: `console_session=${cookie}` } }));
  assert.equal(response.status, 200);
  return response.headers.get("x-console-transcript-scope");
}

const chat = (id: string, store: string) => ({ id, store, title: id, turns: [{ text: `${id}-answer-row` }], updatedAt: 1 });

// spec: surface.open-console.saved-transcript@0ee3fe27
test("Query restores a saved transcript only for its operator, reading session and a listed store", async () => {
  const alice = await scopeFor("alice", "task-1");
  assert.ok(alice && alice.length >= 32);
  assert.equal(await scopeFor("alice", "task-1"), alice);
  assert.ok(!alice.includes("alice") && !alice.includes("task-1"));
  const bob = await scopeFor("bob", "task-1");
  const aliceElsewhere = await scopeFor("alice", "task-2");
  assert.notEqual(bob, alice);
  assert.notEqual(aliceElsewhere, alice);

  const saved = saveSaved(alice, [chat("kept", "field-notes"), chat("revoked", "private-store")]);
  assert.deepEqual(restoreSaved<{ id: string; store: string }>(saved, alice, stores).map((session) => session.id), ["kept"]);
  assert.deepEqual(restoreSaved(saved, bob!, stores), []);
  assert.deepEqual(restoreSaved(saved, aliceElsewhere!, stores), []);
  assert.deepEqual(restoreSaved(saved, alice, []), []);
  assert.deepEqual(restoreSaved(saved, "", stores), []);
  assert.deepEqual(restoreSaved(JSON.stringify([chat("legacy", "field-notes")]), alice, stores), []);
  assert.deepEqual(restoreSaved("not json", alice, stores), []);
  assert.deepEqual(restoreSaved(null, alice, stores), []);
});

// spec: surface.open-console.narrow-sidebar@87b4cd5a
test("Query keeps chat controls reachable below the sidebar breakpoint", () => {
  const html = readFileSync(new URL("../client/dist/query.html", import.meta.url), "utf8");
  assert.match(html, /<button[^>]*aria-controls="console-sidebar"[^>]*aria-label="Open chats"/);
  assert.match(html, /<aside id="console-sidebar"/);
  assert.doesNotMatch(html, /aria-label="Delete chat"[^>]*class="[^"]*(?<!md:)opacity-0/);
});
