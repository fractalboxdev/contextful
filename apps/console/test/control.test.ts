import test from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSync, sign } from "node:crypto";
import { createConsole } from "../src/index.ts";
import { createLiveControl } from "../src/control.ts";

const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const token = (() => {
  const header = Buffer.from(JSON.stringify({ alg: "RS256" })).toString("base64url");
  const claims = Buffer.from(JSON.stringify({ iss: "https://access.example", aud: "admin-app", sub: "operator-1", exp: Math.floor(Date.now() / 1000) + 300 })).toString("base64url");
  return `${header}.${claims}.${sign("RSA-SHA256", Buffer.from(`${header}.${claims}`), privateKey).toString("base64url")}`;
})();

test("Admin route retrieves applied workflow state and applies through the server capability", async () => {
  const calls: { path: string; authorization: string | null; body: string }[] = [];
  const fetcher: typeof fetch = async (input, init) => {
    const url = new URL(String(input));
    calls.push({ path: url.pathname, authorization: new Headers(init?.headers).get("authorization"), body: String(init?.body ?? "") });
    return Response.json(url.pathname === "/control/workflows" ?
      { applied: 1, pipelines: [{ id: "filings-flow", tables: ["filings"], schedule: "every 1h" }], runs: {} } :
      { applied: 2 });
  };
  const control = createLiveControl({ stores: [{ id: "field-notes", endpoint: "http://127.0.0.1:9999" }], capability: "engine-admin-secret", fetcher });
  const app = createConsole({
    identity: { kind: "access", issuer: "https://access.example", queryAudience: "query-app", adminAudience: "admin-app", publicKey },
    stores: [{ id: "field-notes", label: "Field notes" }], adminCapability: "engine-admin-secret",
    turn: async () => ({ answer: "", sources: [], widgets: [] }), read: { list: async () => [] }, control,
  });
  const headers = { "cf-access-jwt-assertion": token };
  const listing = await app.fetch(new Request("https://console.example/admin/api/workflows", { headers }));
  assert.equal(listing.status, 200);
  assert.equal((await listing.json() as { pipelines: { id: string }[] }).pipelines[0].id, "filings-flow");
  const apply = await app.fetch(new Request("https://console.example/admin/api/apply", {
    method: "POST", headers: { ...headers, origin: "https://console.example" }, body: JSON.stringify({ store: "field-notes", id: "filings-flow" }),
  }));
  assert.equal(apply.status, 200);
  assert.deepEqual(calls, [
    { path: "/control/workflows", authorization: "Bearer engine-admin-secret", body: "" },
    { path: "/control/apply", authorization: "Bearer engine-admin-secret", body: '{"id":"filings-flow"}' },
  ]);
  assert.doesNotMatch(JSON.stringify(await apply.json()), /engine-admin-secret/);
});

test("Admin control refuses ambiguous stores and keeps edit unavailable", async () => {
  const control = createLiveControl({
    stores: [{ id: "one", endpoint: "http://127.0.0.1:1" }, { id: "two", endpoint: "http://127.0.0.1:2" }],
    capability: "secret", fetcher: async () => { throw new Error("network must not be called"); },
  });
  await assert.rejects(control.workflows({ subject: "operator", grants: new Set(["admin"]) }, null), /ConsoleStoreSelectionRequired/);
  await assert.rejects(control.edit({}, "secret", { subject: "operator", grants: new Set(["admin"]) }), /ConsoleAdapterUnavailable/);
});
