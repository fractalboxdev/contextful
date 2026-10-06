import test from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { createConsole, issueCognitoSession } from "../src/index.ts";

const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const issuer = "https://access.example";
const queryAudience = "query-app";
const adminAudience = "admin-app";

function jwt(payload: Record<string, unknown>): string {
  const header = Buffer.from(JSON.stringify({ alg: "RS256", typ: "JWT" })).toString("base64url");
  const claims = Buffer.from(JSON.stringify(payload)).toString("base64url");
  return `${header}.${claims}.${sign("RSA-SHA256", Buffer.from(`${header}.${claims}`), privateKey).toString("base64url")}`;
}

function access(aud: string, extra: Record<string, unknown> = {}): HeadersInit {
  return { "cf-access-jwt-assertion": jwt({ iss: issuer, aud, sub: "operator-1", exp: Math.floor(Date.now() / 1000) + 300, ...extra }) };
}

function fixture(adminCapability = "server-secret") {
  const calls: string[] = [];
  const console = createConsole({
    identity: { kind: "access", issuer, queryAudience, adminAudience, publicKey },
    adminCapability,
    stores: [{ id: "field-notes", label: "Field notes" }],
    turn: async ({ question, operator, store }) => {
      calls.push(`turn:${operator.subject}:${store}:${question}`);
      return { answer: "Three filings arrived.", sources: [{ title: "Filings", url: "https://example.test/filings" }], widgets: [{ component: "table.v1", props: { columns: ["Name"], rows: [["One"]] } }] };
    },
    read: { list: async () => [{ id: "field-notes", label: "Field notes" }] },
    control: {
      workflows: async () => { calls.push("workflows"); return { pipelines: [{ id: "filings", steps: ["fetch"], schedule: "every 1h", runs: [] }], annotations: [] }; },
      record: async () => ({ packs: [], learnings: [] }),
      edit: async (_body, capability) => { calls.push(`edit:${capability}`); return { version: 2 }; },
      apply: async (_body, capability) => { calls.push(`apply:${capability}`); return { version: 3 }; },
    },
  });
  const request = (path: string, headers?: HeadersInit, init: RequestInit = {}) => console.fetch(new Request(`https://console.example${path}`, { ...init, headers }));
  return { request, calls };
}

test("anonymous and forged Access assertions dispatch no page or API", async () => {
  const { request, calls } = fixture();
  for (const path of ["/query", "/query/api/stores", "/admin", "/admin/api/workflows"]) {
    assert.equal((await request(path)).status, 403);
    assert.equal((await request(path, { "cf-access-jwt-assertion": "forged" })).status, 403);
  }
  assert.deepEqual(calls, []);
});

test("verified Access audiences grant only their page and API", async () => {
  const { request, calls } = fixture();
  assert.equal((await request("/query", access(queryAudience))).status, 200);
  assert.equal((await request("/query/api/stores", access(queryAudience))).status, 200);
  assert.equal((await request("/admin", access(queryAudience))).status, 403);
  assert.equal((await request("/admin/api/workflows", access(queryAudience))).status, 403);
  assert.equal((await request("/query", access(adminAudience))).status, 403);
  assert.equal((await request("/admin", access(adminAudience))).status, 200);
  assert.equal((await request("/admin/api/workflows", access(adminAudience))).status, 200);
  assert.deepEqual(calls, ["workflows"]);
});

test("expired or wrong-issuer assertions cannot open a page", async () => {
  const { request } = fixture();
  assert.equal((await request("/query", access(queryAudience, { exp: 1 }))).status, 403);
  assert.equal((await request("/query", access(queryAudience, { iss: "https://other.example" }))).status, 403);
});

test("Query keeps one composer and transcript and delegates a sourced answer", async () => {
  const { request, calls } = fixture();
  const page = await (await request("/query", access(queryAudience))).text();
  assert.match(page, /id="composer"/);
  assert.match(page, /id="transcript"/);
  assert.match(page, /id="widgets"/);
  const answer = await request("/query/api/ask", access(queryAudience), { method: "POST", body: JSON.stringify({ store: "field-notes", question: "What arrived?" }) });
  assert.equal(answer.status, 200);
  assert.equal((await answer.json() as { sources: unknown[] }).sources.length, 1);
  assert.deepEqual(calls, ["turn:operator-1:field-notes:What arrived?"]);
});

test("Admin exposes workflows and requires a server-held capability for edit and apply", async () => {
  const { request, calls } = fixture();
  const workflows = await request("/admin/api/workflows", access(adminAudience));
  assert.equal((await workflows.json() as { pipelines: unknown[] }).pipelines.length, 1);
  const edit = await request("/admin/api/edit", access(adminAudience), { method: "POST", body: "{}" });
  assert.equal(edit.status, 200);
  const apply = await request("/admin/api/apply", access(adminAudience), { method: "POST", body: "{}" });
  assert.equal(apply.status, 200);
  assert.deepEqual(calls, ["workflows", "edit:server-secret", "apply:server-secret"]);
  const withoutCapability = fixture("");
  const refused = await withoutCapability.request("/admin/api/apply", access(adminAudience), { method: "POST", body: "{}" });
  assert.equal(refused.status, 403);
  assert.match(await refused.text(), /ConsoleAdminGrantMissing/);
  assert.deepEqual(withoutCapability.calls, []);
});

test("Cognito sessions are signed, bounded, and mapped from groups", async () => {
  const session = issueCognitoSession({ subject: "operator-2", groups: ["console-query"], expiresAt: Math.floor(Date.now() / 1000) + 60 }, "session-secret");
  const { request, calls } = (() => {
    const calls: string[] = [];
    const app = createConsole({
      identity: { kind: "cognito", sessionSecret: "session-secret", queryGroup: "console-query", adminGroup: "console-admin" },
      adminCapability: "server-secret",
      stores: [],
      turn: async () => ({ answer: "", sources: [], widgets: [] }),
      read: { list: async () => [] },
      control: { workflows: async () => { calls.push("workflows"); return {}; }, record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
    });
    return { request: (path: string, cookie: string) => app.fetch(new Request(`https://console.example${path}`, { headers: { cookie } })), calls };
  })();
  assert.equal((await request("/query", `console_session=${session}`)).status, 200);
  assert.equal((await request("/admin/api/workflows", `console_session=${session}`)).status, 403);
  assert.equal((await request("/query", `console_session=${session}tampered`)).status, 403);
  assert.deepEqual(calls, []);
});
