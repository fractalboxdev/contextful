import assert from "node:assert/strict";
import { test } from "node:test";
import { handleExecutionBinding, type ExecutionBindings } from "../src/binding.ts";

const endpoint = "http://contextful.d1";

test("the private route refuses absent D1 and R2 bindings", async () => {
  const response = await handleExecutionBinding(new Request(`${endpoint}/health`), {});
  assert.equal(response.status, 503);
  assert.deepEqual(await response.json(), { error: "StorageCapabilityMissing" });
});

test("a different host cannot invoke the binding", async () => {
  const response = await handleExecutionBinding(new Request("http://public.example/health"), {});
  assert.equal(response.status, 404);
});

test("queries and batches use the D1 binding with ordered parameters", async () => {
  const calls: unknown[] = [];
  const db = {
    prepare(sql: string) {
      return {
        bind(...params: (string | number | null)[]) {
          calls.push({ sql, params });
          return { run: async () => ({ results: [{ value: params[0] }], meta: { changes: 1 } }) };
        },
      };
    },
    async batch(statements: unknown[]) {
      calls.push({ batch: statements.length });
      return statements.map(() => ({ results: [], meta: { changes: 1 } }));
    },
  };
  const env = { EXECUTION_DB: db, EXECUTION_BLOBS: { put: async () => {}, get: async () => null, head: async () => null, delete: async () => {} } } satisfies ExecutionBindings;
  const single = await handleExecutionBinding(new Request(`${endpoint}/sql`, {
    method: "POST", body: JSON.stringify({ sql: "SELECT ? AS value", params: [7] }),
  }), env);
  assert.equal(single.status, 200);
  assert.deepEqual(await single.json(), { rows: [{ value: 7 }], changes: 1 });
  const batch = await handleExecutionBinding(new Request(`${endpoint}/batch`, {
    method: "POST", body: JSON.stringify([{ sql: "INSERT INTO t VALUES (?)", params: ["a"] }, { sql: "INSERT INTO t VALUES (?)", params: ["b"] }]),
  }), env);
  assert.equal(batch.status, 200);
  assert.deepEqual(calls, [
    { sql: "SELECT ? AS value", params: [7] },
    { sql: "INSERT INTO t VALUES (?)", params: ["a"] },
    { sql: "INSERT INTO t VALUES (?)", params: ["b"] },
    { batch: 2 },
  ]);
});

test("content-addressed blob transport keeps bytes outside D1", async () => {
  const objects = new Map<string, Uint8Array>();
  const env = {
    EXECUTION_DB: { prepare: () => ({ bind: () => ({ run: async () => ({ results: [], meta: { changes: 0 } }) }) }), batch: async () => [] },
    EXECUTION_BLOBS: {
      async put(key: string, body: ReadableStream<Uint8Array> | ArrayBuffer) {
        const bytes = body instanceof ArrayBuffer ? new Uint8Array(body) : new Uint8Array(await new Response(body).arrayBuffer());
        objects.set(key, bytes);
      },
      async get(key: string) {
        const bytes = objects.get(key);
        return bytes ? { body: new Blob([Uint8Array.from(bytes).buffer]).stream() as ReadableStream<Uint8Array> } : null;
      },
      async head(key: string) { return objects.get(key) ?? null; },
      async delete(key: string) { objects.delete(key); },
    },
  } satisfies ExecutionBindings;
  const digest = "a".repeat(64);
  const put = await handleExecutionBinding(new Request(`${endpoint}/blob/${digest}`, { method: "PUT", body: "payload" }), env);
  assert.equal(put.status, 204);
  const get = await handleExecutionBinding(new Request(`${endpoint}/blob/${digest}`), env);
  assert.equal(await get.text(), "payload");
  const head = await handleExecutionBinding(new Request(`${endpoint}/blob/${digest}`, { method: "HEAD" }), env);
  assert.equal(head.status, 204);
  const missing = await handleExecutionBinding(new Request(`${endpoint}/blob/${"b".repeat(64)}`, { method: "HEAD" }), env);
  assert.equal(missing.status, 404);
  assert.equal(objects.size, 1);
  assert.equal(objects.has(`blobs/${digest}`), true);
});
