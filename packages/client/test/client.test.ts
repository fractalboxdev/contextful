import assert from "node:assert/strict";
import { chmodSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
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

test("engine-direct sends JSON-RPC to a real HTTP transport with its viewer credential", async () => {
  const server = createServer(async (request, response) => {
    assert.equal(request.url, "/mcp");
    assert.equal(request.method, "POST");
    assert.equal(request.headers.authorization, "Bearer viewer-token");
    assert.equal(request.headers.accept, "application/json");
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(chunk);
    assert.equal(JSON.parse(Buffer.concat(chunks).toString())["method"], "tools/list");
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify({ jsonrpc: "2.0", id: 1, result: { tools: [] } }));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const address = server.address();
    assert(address && typeof address !== "string");
    const client = createClient({ shape: "engine-direct", baseUrl: `http://127.0.0.1:${address.port}/mcp`, token: "viewer-token" });
    assert.deepEqual(await client.call("tools/list"), { tools: [] });
  } finally {
    server.close();
  }
});

test("HTTP client rejects an in-band MCP tool refusal", async () => {
  const client = createClient({
    shape: "engine-direct",
    baseUrl: "https://engine.example.test/mcp",
    token: "viewer-token",
    fetch: async () => Response.json({
      jsonrpc: "2.0",
      id: 1,
      result: {
        isError: true,
        content: [{ type: "text", text: "scope denied" }],
        structuredContent: { error: { identifier: "ScopeDenied", message: "scope denied" } },
      },
    }),
  });
  await assert.rejects(client.call("tools/call"), { name: "ScopeDenied", message: /scope denied/ });
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

test("spawned client rejects an in-band MCP tool refusal", async () => {
  const root = mkdtempSync(join(tmpdir(), "contextful-client-"));
  writeFileSync(join(root, "contextful.toml"), "");
  const fixture = join(root, "refused-mcp");
  writeFileSync(fixture, "#!/bin/sh\nread line\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"isError\":true,\"content\":[{\"type\":\"text\",\"text\":\"scope denied\"}],\"structuredContent\":{\"error\":{\"identifier\":\"ScopeDenied\",\"message\":\"scope denied\"}}}}'\n");
  chmodSync(fixture, 0o700);
  const client = new SpawnedClient({ cwd: root, command: fixture, token: "reader-token" });
  await assert.rejects(client.call("tools/call"), { name: "ScopeDenied", message: /scope denied/ });
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
