import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { registryFromEnv, routeRequest } from "../src/index.ts";

// spec: topology.publish-hostname.unconfigured-gateway@4bb428f8
test("an empty registry returns GatewayUnconfigured on every route", async () => {
  const registry = registryFromEnv("[]");
  for (const path of ["/query", "/", "/stores/field-notes/mcp", "/health"]) {
    const response = await routeRequest(new Request(`https://gateway.example${path}`, { method: "POST", body: "{}" }), registry);
    assert.equal(response.status, 503, path);
    assert.match(await response.text(), /GatewayUnconfigured/, path);
  }
});

// spec: surface.register-store.registry-unreadable@b128ea60
test("an unreadable registry returns StoreRegistryUnreadable", () => {
  assert.throws(() => registryFromEnv("[\"field-notes\""), /StoreRegistryUnreadable/);
  assert.throws(() => registryFromEnv(JSON.stringify({ id: "field-notes" })), /StoreRegistryUnreadable/);
});

// spec: surface.register-store.malformed-entry@fe2a3d82
test("an invalid optional field is isolated with its entry", () => {
  const registry = registryFromEnv(JSON.stringify([
    { id: "bad", endpoint: "https://bad.example", auth: 42 },
    { id: "good", endpoint: "https://read.example/base" },
  ]));
  assert.deepEqual(registry.problems, [{ identifier: "StoreEntryMalformed", entry: 0 }]);
  assert.equal(registry.entries.length, 1);
  assert.equal(registry.entries[0].id, "good");
});

test("a malformed entry is dropped while a valid sibling routes", async () => {
  const registry = registryFromEnv(JSON.stringify([
    { id: "bad id", endpoint: "https://bad.example" },
    { id: "field-notes", label: "Field notes", endpoint: "https://read.example" },
  ]));
  assert.equal(registry.entries.length, 1);
  assert.equal(registry.entries[0].credentialName, "FIELD_NOTES_QUERY_TOKEN");
  const response = await routeRequest(new Request("https://gateway.example/stores/field-notes/mcp", {
    headers: { authorization: "Bearer opaque-token" },
  }), registry, async (_entry, request) => new Response(request.headers.get("authorization")));
  assert.equal(response.status, 200);
  assert.equal(await response.text(), "Bearer opaque-token");
});

test("routing preserves the configured endpoint path", async () => {
  const registry = registryFromEnv(JSON.stringify([{ id: "field-notes", endpoint: "https://read.example/base" }]));
  let target = "";
  await routeRequest(new Request("https://gateway.example/stores/field-notes/mcp?limit=1"), registry, async (_entry, request) => {
    target = request.url;
    return new Response(null, { status: 204 });
  });
  assert.equal(target, "https://read.example/base/mcp?limit=1");
});

// spec: surface.register-store.reserved-id@edc6b4fc
test("an entry claiming a built-in id returns StoreIdReserved", () => {
  assert.throws(() => registryFromEnv(JSON.stringify([{ id: "query", endpoint: "https://read.example" }])), /StoreIdReserved/);
  assert.throws(() => registryFromEnv(JSON.stringify([{ id: "admin", endpoint: "https://read.example" }])), /StoreIdReserved/);
});

// spec: surface.register-store.authored-name@6fb4da24
test("an entry authoring its credential or binding name returns StoreNameAuthored", () => {
  for (const key of ["credential", "credentialName", "binding", "bindingName"]) {
    assert.throws(() => registryFromEnv(JSON.stringify([{ id: "field-notes", endpoint: "https://read.example", [key]: "OTHER" }])), /StoreNameAuthored/, key);
  }
});

// spec: surface.register-store.shared-registry@e3840a26
test("the console resolves store identities through the gateway registry", () => {
  const consoleEntry = readFileSync(new URL("../../console/src/entry.ts", import.meta.url), "utf8");
  assert.match(consoleEntry, /import \{ registryFromEnv \} from "\.\.\/\.\.\/gateway\/src\/index\.ts";/);
  assert.match(consoleEntry, /registryFromEnv\(process\.env\.CONTEXTFUL_STORES_JSON\)/);
  assert.doesNotMatch(consoleEntry, /JSON\.parse\(process\.env\.CONTEXTFUL_STORES_JSON/);
  const registry = registryFromEnv(JSON.stringify([{ id: "field-notes", label: "Field notes", endpoint: "https://read.example" }]));
  assert.deepEqual(registry.entries.map(({ id, label }) => ({ id, label })), [{ id: "field-notes", label: "Field notes" }]);
});
