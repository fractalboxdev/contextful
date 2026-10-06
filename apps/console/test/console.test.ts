import test from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { createConsole, issueCognitoSession } from "../src/index.ts";
import { serveConsole } from "../src/server.ts";

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
    assert.equal((await request(path)).status, 401);
    assert.equal((await request(path, { "cf-access-jwt-assertion": "forged" })).status, 401);
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
  assert.equal((await request("/query", access(queryAudience, { exp: 1 }))).status, 401);
  assert.equal((await request("/query", access(queryAudience, { iss: "https://other.example" }))).status, 401);
});

test("Query keeps one composer and transcript and delegates a sourced answer", async () => {
  const { request, calls } = fixture();
  const page = await (await request("/query", access(queryAudience))).text();
  assert.match(page, /id="composer"/);
  assert.match(page, /id="transcript"/);
  assert.match(page, /id="widgets"/);
  const answer = await request("/query/api/ask", { ...access(queryAudience), origin: "https://console.example" }, { method: "POST", body: JSON.stringify({ store: "field-notes", question: "What arrived?" }) });
  assert.equal(answer.status, 200);
  assert.equal((await answer.json() as { sources: unknown[] }).sources.length, 1);
  assert.deepEqual(calls, ["turn:operator-1:field-notes:What arrived?"]);
});

test("Access Query rejects cross-origin and originless POST before a turn", async () => {
  const { request, calls } = fixture();
  const body = JSON.stringify({ store: "field-notes", question: "What arrived?" });
  for (const headers of [access(queryAudience), { ...access(queryAudience), origin: "https://other.example" }]) {
    assert.equal((await request("/query/api/ask", headers, { method: "POST", body })).status, 403);
  }
  assert.deepEqual(calls, []);
});

test("Admin exposes workflows and requires a server-held capability for edit and apply", async () => {
  const { request, calls } = fixture();
  const workflows = await request("/admin/api/workflows", access(adminAudience));
  assert.equal((await workflows.json() as { pipelines: unknown[] }).pipelines.length, 1);
  const origin = "https://console.example";
  const edit = await request("/admin/api/edit", { ...access(adminAudience), origin }, { method: "POST", body: "{}" });
  assert.equal(edit.status, 200);
  const apply = await request("/admin/api/apply", { ...access(adminAudience), origin }, { method: "POST", body: "{}" });
  assert.equal(apply.status, 200);
  assert.deepEqual(calls, ["workflows", "edit:server-secret", "apply:server-secret"]);
  assert.equal((await request("/admin/api/edit", access(adminAudience), { method: "POST", body: "{}" })).status, 403);
  assert.equal((await request("/admin/api/edit", { ...access(adminAudience), origin: "https://other.example" }, { method: "POST", body: "{}" })).status, 403);
  assert.deepEqual(calls, ["workflows", "edit:server-secret", "apply:server-secret"]);
  const withoutCapability = fixture("");
  const refused = await withoutCapability.request("/admin/api/apply", { ...access(adminAudience), origin }, { method: "POST", body: "{}" });
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
  assert.equal((await request("/query", `console_session=${session}tampered`)).status, 401);
  assert.deepEqual(calls, []);
});

test("Cognito hosted login exchanges a code before issuing a first-party session", async () => {
  const app = createConsole({
    identity: {
      kind: "cognito", sessionSecret: "session-secret", queryGroup: "console-query", adminGroup: "console-admin",
      issuer: "https://cognito.example/pool", clientId: "console-client", publicKey,
      authorizeUrl: "https://cognito.example/oauth2/authorize", tokenUrl: "https://cognito.example/oauth2/token",
      redirectUri: "https://console.example/auth/callback",
      fetcher: async (_url, init) => {
        assert.equal(init?.method, "POST");
        assert.match(String(init?.body), /code=accepted/);
        assert.match(String(init?.body), /code_verifier=/);
        return Response.json({ id_token: jwt({ iss: "https://cognito.example/pool", aud: "console-client", sub: "operator-3", token_use: "id", "cognito:groups": ["console-admin"], exp: Math.floor(Date.now() / 1000) + 300 }) });
      },
    },
    stores: [], turn: async () => ({ answer: "", sources: [], widgets: [] }), read: { list: async () => [] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  const login = await app.fetch(new Request("https://console.example/auth/login"));
  assert.equal(login.status, 302);
  const location = new URL(login.headers.get("location")!);
  const state = location.searchParams.get("state");
  assert.ok(state);
  assert.equal(location.searchParams.get("code_challenge_method"), "S256");
  assert.match(location.searchParams.get("code_challenge") ?? "", /^[A-Za-z0-9_-]{43}$/);
  const stateCookie = login.headers.get("set-cookie")!.split(";")[0];
  const callback = await app.fetch(new Request(`https://console.example/auth/callback?code=accepted&state=${state}`, { headers: { cookie: stateCookie } }));
  assert.equal(callback.status, 302);
  assert.equal(callback.headers.get("location"), "/admin");
  const sessionCookie = callback.headers.get("set-cookie")!.split(";")[0];
  assert.equal((await app.fetch(new Request("https://console.example/admin", { headers: { cookie: sessionCookie } }))).status, 200);
  assert.equal((await app.fetch(new Request("https://console.example/query", { headers: { cookie: sessionCookie } }))).status, 403);
  const badState = await app.fetch(new Request("https://console.example/auth/callback?code=accepted&state=wrong", { headers: { cookie: stateCookie } }));
  assert.equal(badState.status, 403);
  const missingVerifier = await app.fetch(new Request(`https://console.example/auth/callback?code=accepted&state=${state}`, { headers: { cookie: `console_login_state=${state}` } }));
  assert.equal(missingVerifier.status, 403);
  const [cookieName, cookieValue] = stateCookie.split("=");
  const tampered = cookieValue.split(".");
  tampered[1] = `${tampered[1].slice(0, -1)}${tampered[1].endsWith("A") ? "B" : "A"}`;
  const wrongVerifier = await app.fetch(new Request(`https://console.example/auth/callback?code=accepted&state=${state}`, { headers: { cookie: `${cookieName}=${tampered.join(".")}` } }));
  assert.equal(wrongVerifier.status, 403);
});

test("Cognito Admin POST uses the listener's bound port for its Origin", async () => {
  let edits = 0;
  const app = serveConsole({
    identity: { kind: "cognito", sessionSecret: "session-secret", queryGroup: "query", adminGroup: "admin" },
    adminCapability: "server-secret",
    stores: [], turn: async () => ({ answer: "", sources: [], widgets: [] }), read: { list: async () => [] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => { edits++; return { version: 1 }; }, apply: async () => ({}) },
  }, () => origin);
  await new Promise<void>((resolve) => app.listen(0, "127.0.0.1", resolve));
  const address = app.address();
  assert.ok(address && typeof address !== "string");
  const origin = `http://127.0.0.1:${address.port}`;
  const session = issueCognitoSession({ subject: "operator", groups: ["admin"], expiresAt: Math.floor(Date.now() / 1000) + 60 }, "session-secret");
  try {
    const response = await fetch(`${origin}/admin/api/edit`, {
      method: "POST", headers: { cookie: `console_session=${session}`, origin, "content-type": "application/json" }, body: "{}",
    });
    assert.equal(response.status, 200);
    assert.equal(edits, 1);
  } finally { app.close(); }
});

test("the Node HTTP listener verifies a page request before serving content", async () => {
  const app = createConsole({
    identity: { kind: "access", issuer, queryAudience, adminAudience, publicKey },
    stores: [],
    turn: async () => ({ answer: "", sources: [], widgets: [] }),
    read: { list: async () => [] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  assert.equal((await app.fetch(new Request("https://console.example/query"))).status, 401);
  const server = serveConsole({
    identity: { kind: "access", issuer, queryAudience, adminAudience, publicKey },
    stores: [],
    turn: async () => ({ answer: "", sources: [], widgets: [] }),
    read: { list: async () => [] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  }, "http://127.0.0.1");
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const address = server.address();
    assert.ok(address && typeof address !== "string");
    const url = `http://127.0.0.1:${address.port}`;
    assert.equal((await fetch(`${url}/query`)).status, 401);
    assert.equal((await fetch(`${url}/query`, { headers: access(queryAudience) })).status, 200);
  } finally {
    server.close();
  }
});

test("the Node HTTP listener rejects an oversized body before dispatch", async () => {
  let turns = 0;
  const server = serveConsole({
    identity: { kind: "access", issuer, queryAudience, adminAudience, publicKey },
    stores: [{ id: "field-notes", label: "Field notes" }],
    turn: async () => { turns++; return { answer: "", sources: [], widgets: [] }; },
    read: { list: async () => [] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  }, "http://127.0.0.1");
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const address = server.address();
    assert.ok(address && typeof address !== "string");
    const response = await fetch(`http://127.0.0.1:${address.port}/query/api/ask`, {
      method: "POST", headers: { "content-type": "application/json" }, body: Buffer.alloc(1_048_577, "x"),
    });
    assert.equal(response.status, 413);
    assert.equal((await response.json() as { error: { identifier: string } }).error.identifier, "ConsoleBodyTooLarge");
    assert.equal(turns, 0);
  } finally { server.close(); }
});

test("Query counts UTF-8 bytes without Content-Length before parsing", async () => {
  const { request, calls } = fixture();
  const body = JSON.stringify({ store: "field-notes", question: "é".repeat(524_289) });
  const response = await request("/query/api/ask", { ...access(queryAudience), origin: "https://console.example" }, { method: "POST", body });
  assert.equal(response.status, 413);
  assert.equal((await response.json() as { error: { identifier: string } }).error.identifier, "ConsoleBodyTooLarge");
  assert.deepEqual(calls, []);
});

test("Query stops reading a chunked body when its byte cap is crossed", async () => {
  const { request, calls } = fixture();
  let pulls = 0;
  const stream = new ReadableStream<Uint8Array>({
    pull(controller) {
      pulls++;
      controller.enqueue(Buffer.alloc(524_289, 120));
      if (pulls === 4) controller.close();
    },
  }, { highWaterMark: 0 });
  const response = await request("/query/api/ask", { ...access(queryAudience), origin: "https://console.example" },
    { method: "POST", body: stream, duplex: "half" } as RequestInit);
  assert.equal(response.status, 413);
  assert.ok(pulls < 4, `read ${pulls} chunks past the cap`);
  assert.deepEqual(calls, []);
});

test("Admin rejects an unknown store before workflow and record adapters", async () => {
  const { request, calls } = fixture();
  for (const path of ["/admin/api/workflows", "/admin/api/record"]) {
    const response = await request(`${path}?store=missing`, access(adminAudience));
    assert.equal(response.status, 400);
    assert.equal((await response.json() as { error: { identifier: string } }).error.identifier, "ConsoleRequestMalformed");
  }
  assert.deepEqual(calls, []);
});

test("Admin pack listing reaches only the injected listing adapter", async () => {
  const calls: string[] = [];
  const app = createConsole({
    identity: { kind: "access", issuer, queryAudience, adminAudience, publicKey },
    stores: [{ id: "field-notes", label: "Field notes" }],
    turn: async () => ({ answer: "", sources: [], widgets: [] }),
    read: { list: async () => [] },
    control: {
      workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}),
      listPackFiles: async (_operator, store, prefix) => { calls.push(`${store}:${prefix}`); return { entries: ["packs/a.toml"], truncated: false, declined: 2 }; },
    },
  });
  const url = "https://console.example/admin/api/packs?store=field-notes&prefix=packs/";
  assert.equal((await app.fetch(new Request(url, { headers: access(queryAudience) }))).status, 403);
  const response = await app.fetch(new Request(url, { headers: access(adminAudience) }));
  assert.deepEqual(await response.json(), { entries: ["packs/a.toml"], truncated: false, declined: 2 });
  assert.deepEqual(calls, ["field-notes:packs/"]);
});

test("Query builds its widget from rows and sanitizes view props before delivery", async () => {
  const app = createConsole({
    identity: { kind: "access", issuer, queryAudience, adminAudience, publicKey },
    stores: [{ id: "field-notes", label: "Field notes" }],
    turn: async () => ({
      answer: "One account arrived.", sources: [],
      widgets: [{ component: "injected.v1", props: { value: "secret@example.test" } }],
      resultRows: { columns: ["account"], rows: [["secret@example.test"]] },
    }),
    redactView: (_operator, value) => value.replace("secret@example.test", "[redacted]"),
    read: { list: async () => [] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  const response = await app.fetch(new Request("https://console.example/query/api/ask", {
    method: "POST", headers: { ...access(queryAudience), origin: "https://console.example" },
    body: JSON.stringify({ store: "field-notes", question: "What arrived?" }),
  }));
  assert.equal(response.status, 200);
  const answer = await response.json() as { widgets: Array<{ component: string; props: { rows: unknown[][] } }> };
  assert.equal(answer.widgets.length, 1);
  assert.equal(answer.widgets[0].component, "table.v1");
  assert.deepEqual(answer.widgets[0].props.rows, [["[redacted]"]]);
});

test("Query refuses a client view and an access-explanation share control", async () => {
  let turns = 0;
  const app = createConsole({
    identity: { kind: "access", issuer, queryAudience, adminAudience, publicKey },
    stores: [{ id: "field-notes", label: "Field notes" }],
    turn: async () => { turns++; return { answer: "Private rationale", sources: [], widgets: [], accessExplanation: true, share: true }; },
    read: { list: async () => [] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  const ask = (extra: Record<string, unknown> = {}) => app.fetch(new Request("https://console.example/query/api/ask", {
    method: "POST", headers: { ...access(queryAudience), origin: "https://console.example" },
    body: JSON.stringify({ store: "field-notes", question: "Why?", ...extra }),
  }));
  const authored = await ask({ view: { component: "metric.v1" } });
  assert.equal(authored.status, 400);
  assert.equal((await authored.json() as { error: { identifier: string } }).error.identifier, "ConsoleViewNotServerBuilt");
  assert.equal(turns, 0);
  const shared = await ask();
  assert.equal(shared.status, 403);
  assert.equal((await shared.json() as { error: { identifier: string } }).error.identifier, "VisibilityShareAffordance");
});

test("Query brief derives a private card only from a turnless present-time session", async () => {
  const now = Date.parse("2026-01-08T12:00:00Z");
  const app = createConsole({
    identity: { kind: "access", issuer, queryAudience, adminAudience, publicKey },
    stores: [{ id: "field-notes", label: "Field notes" }],
    turn: async () => ({ answer: "", sources: [], widgets: [] }),
    brief: async () => ({ session: { turns: 0, vantage: "present" }, now,
      conclusions: [{ subject: "Acme", text: "Acme filings need review", live: true }],
      arrivals: [{ id: "a", label: "Acme filing", topics: ["filings"], arrivedAt: "2026-01-08T11:00:00Z" }],
    }),
    read: { list: async () => [] },
    control: { workflows: async () => ({}), record: async () => ({}), edit: async () => ({}), apply: async () => ({}) },
  });
  const url = "https://console.example/query/api/brief?store=field-notes";
  assert.equal((await app.fetch(new Request(url))).status, 401);
  const response = await app.fetch(new Request(url, { headers: access(queryAudience) }));
  assert.equal(response.status, 200);
  assert.equal((await response.json() as { subjects: Array<{ subject: string }> }).subjects[0].subject, "Acme");
  assert.equal((await app.fetch(new Request("https://console.example/query/api/brief?store=missing", { headers: access(queryAudience) }))).status, 400);
});
