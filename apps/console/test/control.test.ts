import test from "node:test";
import assert from "node:assert/strict";
import { createHash, createHmac, generateKeyPairSync, sign } from "node:crypto";
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
      url.pathname === "/control/record" ? { entries: [], truncated: false, declined: 0 } :
      url.pathname === "/control/edit" ? { expected: 1, nonce: "a".repeat(48) } : { applied: 2 });
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
  const record = await app.fetch(new Request("https://console.example/admin/api/record", { headers }));
  assert.equal(record.status, 200);
  const edit = await app.fetch(new Request("https://console.example/admin/api/edit", {
    method: "POST", headers: { ...headers, origin: "https://console.example" },
    body: JSON.stringify({ store: "field-notes", expected: 1, document: "[[pipeline]]\nid = \"filings-flow\"" }),
  }));
  assert.equal(edit.status, 200);
  const apply = await app.fetch(new Request("https://console.example/admin/api/apply", {
    method: "POST", headers: { ...headers, origin: "https://console.example" }, body: JSON.stringify({ store: "field-notes", expected: 1, nonce: "a".repeat(48) }),
  }));
  assert.equal(apply.status, 200);
  assert.deepEqual(calls, [
    { path: "/control/workflows", authorization: "Bearer engine-admin-secret", body: "" },
    { path: "/control/record", authorization: "Bearer engine-admin-secret", body: "" },
    { path: "/control/edit", authorization: "Bearer engine-admin-secret", body: '{"expected":1,"document":"[[pipeline]]\\nid = \\"filings-flow\\""}' },
    { path: "/control/apply", authorization: "Bearer engine-admin-secret", body: '{"expected":1,"nonce":"' + "a".repeat(48) + '"}' },
  ]);
  assert.doesNotMatch(JSON.stringify(await apply.json()), /engine-admin-secret/);
});

test("Admin control refuses ambiguous stores", async () => {
  const control = createLiveControl({
    stores: [{ id: "one", endpoint: "http://127.0.0.1:1" }, { id: "two", endpoint: "http://127.0.0.1:2" }],
    capability: "secret", fetcher: async () => { throw new Error("network must not be called"); },
  });
  await assert.rejects(control.workflows({ subject: "operator", grants: new Set(["admin"]) }, null), /ConsoleStoreSelectionRequired/);
  await assert.rejects(control.edit({ expected: 1, document: "" }, "secret", { subject: "operator", grants: new Set(["admin"]) }), /ConsoleStoreSelectionRequired/);
});

test("registered stores share one Admin capability and console attestation domain", async () => {
  const seen: string[] = [];
  const control = createLiveControl({
    stores: [{ id: "one", endpoint: "https://one.example" }, { id: "two", endpoint: "https://two.example" }],
    capability: "deployment-admin", attestationSecret: "deployment-attestation",
    fetcher: async (input, init) => {
      const url = new URL(String(input));
      const headers = new Headers(init?.headers);
      assert.equal(headers.get("authorization"), "Bearer deployment-admin");
      const body = String(init?.body);
      const canonical = `POST\n${url.pathname}\n${createHash("sha256").update(body).digest("hex")}\noperator\n${headers.get("x-contextful-operator-time")}\n${headers.get("x-contextful-operator-nonce")}`;
      assert.equal(headers.get("x-contextful-operator-signature"), createHmac("sha256", "deployment-attestation").update(canonical).digest("hex"));
      seen.push(url.hostname);
      return Response.json({ expected: 1, nonce: "a".repeat(48) });
    },
  });
  const operator = { subject: "operator", grants: new Set<"admin">(["admin"]) };
  await control.edit({ store: "one", expected: 1, document: "" }, "deployment-admin", operator);
  await control.edit({ store: "two", expected: 1, document: "" }, "deployment-admin", operator);
  assert.deepEqual(seen, ["one.example", "two.example"]);
});

test("the live control adapter signs the verified subject and exact request body", async () => {
  let signed = false;
  const control = createLiveControl({ stores: [{ id: "one", endpoint: "http://127.0.0.1:1" }],
    capability: "admin-capability", attestationSecret: "separate-attestation-secret",
    fetcher: async (input, init) => {
      const headers = new Headers(init?.headers);
      const body = String(init?.body);
      const subject = headers.get("x-contextful-operator");
      assert.equal(subject, "verified-operator");
      const canonical = `POST\n${new URL(String(input)).pathname}\n${createHash("sha256").update(body).digest("hex")}\n${subject}\n${headers.get("x-contextful-operator-time")}\n${headers.get("x-contextful-operator-nonce")}`;
      assert.equal(headers.get("x-contextful-operator-signature"), createHmac("sha256", "separate-attestation-secret").update(canonical).digest("hex"));
      signed = true;
      return Response.json({ expected: 1, nonce: "a".repeat(48) });
    },
  });
  await control.edit({ store: "one", expected: 1, document: "" }, "admin-capability", { subject: "verified-operator", grants: new Set(["admin"]) });
  assert.equal(signed, true);
  await assert.rejects(control.edit({ store: "one", expected: 1, document: "", operator: "forged" }, "admin-capability",
    { subject: "verified-operator", grants: new Set(["admin"]) }), /ConsoleRequestMalformed/);
});
