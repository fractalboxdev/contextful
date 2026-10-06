import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { createClient, listPackKeys, SpawnedClient } from "../src/index.ts";

test("four client shapes carry credentials in their assigned transport", async () => {
  const calls: Request[] = [];
  const fetcher = async (request: Request) => {
    calls.push(request);
    return Response.json({ jsonrpc: "2.0", id: 1, result: { ok: true } });
  };
  const proxy = createClient({ shape: "same-origin-proxy", baseUrl: "https://console.example.test/query/stores/field-notes", fetch: fetcher });
  const direct = createClient({ shape: "engine-direct", baseUrl: "https://engine.example.test/mcp", token: "viewer-token", fetch: fetcher });
  const binding = createClient({ shape: "service-binding", binding: { fetch: fetcher }, baseUrl: "https://binding.invalid/mcp" });
  assert.deepEqual(await proxy.call("tools/list"), { ok: true });
  assert.deepEqual(await direct.call("tools/list"), { ok: true });
  assert.deepEqual(await binding.call("tools/list"), { ok: true });
  assert.equal(calls[0].headers.get("authorization"), null);
  assert.equal(calls[1].headers.get("authorization"), "Bearer viewer-token");
  assert.equal(calls[2].headers.get("authorization"), null);
  assert.equal(calls[0].url, "https://console.example.test/query/stores/field-notes/mcp");
});

test("spawned child refuses absent credential and absent ancestor manifest before framing", async () => {
  const root = mkdtempSync(join(tmpdir(), "contextful-client-"));
  const nested = join(root, "a", "b");
  mkdirSync(nested, { recursive: true });
  assert.throws(() => new SpawnedClient({ cwd: nested, command: "contextful" }), /StdioCredentialMissing/);
  assert.throws(() => new SpawnedClient({ cwd: nested, command: "contextful", token: "reader-token" }), /StoreSelectorAbsent/);
  writeFileSync(join(root, "contextful.toml"), "");
  const client = new SpawnedClient({ cwd: nested, command: "contextful", token: "reader-token" });
  assert.equal(client.projectRoot, root);
  assert.equal(client.command, "contextful");
  const fixture = join(root, "echo-mcp");
  writeFileSync(fixture, "#!/bin/sh\nread line\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}'\n");
  chmodSync(fixture, 0o700);
  const spawned = new SpawnedClient({ cwd: nested, command: fixture, token: "reader-token" });
  assert.deepEqual(await spawned.call("tools/list"), { ok: true });
});

test("listing counts declined keys and flags the first omitted entry", () => {
  const keys = Array.from({ length: 1005 }, (_, index) => `packs/${String(index).padStart(4, "0")}`);
  keys.push("private/a", "private/b");
  const page = listPackKeys(keys, (key) => !key.startsWith("private/"));
  assert.equal(page.entries.length, 1000);
  assert.equal(page.truncated, true);
  assert.equal(page.declined, 2);
  const exact = listPackKeys(keys.slice(0, 1000), () => true);
  assert.equal(exact.truncated, false);
  assert.equal(exact.entries.length, 1000);
});
