import test from "node:test";
import assert from "node:assert/strict";
import { registryFromEnv, routeRequest } from "../src/index.ts";

test("an empty registry returns GatewayUnconfigured on every route", async () => {
  const registry = registryFromEnv("[]");
  const response = await routeRequest(new Request("https://gateway.example/query"), registry);
  assert.equal(response.status, 503);
  assert.match(await response.text(), /GatewayUnconfigured/);
});

test("an unreadable registry returns StoreRegistryUnreadable", async () => {
  assert.throws(() => registryFromEnv("{"), /StoreRegistryUnreadable/);
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

test("reserved and authored names refuse their entries", () => {
  assert.throws(() => registryFromEnv(JSON.stringify([{ id: "query", endpoint: "https://read.example" }])), /StoreIdReserved/);
  assert.throws(() => registryFromEnv(JSON.stringify([{ id: "field-notes", endpoint: "https://read.example", binding: "OTHER" }])), /StoreNameAuthored/);
});
