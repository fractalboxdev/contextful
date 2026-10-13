/** The container reaches these bindings only through its outbound virtual host. */
export interface ExecutionBindings {
  EXECUTION_DB?: {
    prepare(sql: string): { bind(...params: (string | number | null)[]): { run(): Promise<{ results: Record<string, unknown>[]; meta: { changes: number } }> } };
    batch(statements: unknown[]): Promise<{ results: Record<string, unknown>[]; meta: { changes: number } }[]>;
  };
  EXECUTION_BLOBS?: {
    put(key: string, body: ReadableStream<Uint8Array> | ArrayBuffer): Promise<unknown>;
    get(key: string): Promise<{ body: ReadableStream<Uint8Array> } | null>;
    delete(key: string): Promise<void>;
  };
}

type Statement = { sql: string; params: (string | number | null)[] };
const DIGEST = /^[a-f0-9]{64}$/;
const VIRTUAL_HOST = "contextful.d1";

function error(identifier: string, status: number): Response {
  return Response.json({ error: identifier }, { status });
}

function statement(value: unknown): value is Statement {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return false;
  const item = value as Record<string, unknown>;
  return typeof item.sql === "string" && item.sql.length > 0 && item.sql.length <= 100_000 &&
    Array.isArray(item.params) && item.params.length <= 100 &&
    item.params.every((param) => param === null || typeof param === "string" ||
      (typeof param === "number" && Number.isSafeInteger(param)));
}

function prepared(db: NonNullable<ExecutionBindings["EXECUTION_DB"]>, query: Statement) {
  return db.prepare(query.sql).bind(...query.params);
}

/** Only the Container class's outboundByHost handler calls this function. */
export async function handleExecutionBinding(request: Request, env: ExecutionBindings): Promise<Response> {
  const url = new URL(request.url);
  if (url.protocol !== "http:" || url.hostname !== VIRTUAL_HOST || url.port) {
    return error("StorageBindingRouteInvalid", 404);
  }
  if (!env.EXECUTION_DB || !env.EXECUTION_BLOBS) {
    return error("StorageCapabilityMissing", 503);
  }
  try {
    if (url.pathname === "/health" && request.method === "GET") {
      await env.EXECUTION_DB.prepare("SELECT 1 AS ready").bind().run();
      return Response.json({ d1: true, r2: true });
    }
    if (url.pathname === "/sql" && request.method === "POST") {
      const query: unknown = await request.json();
      if (!statement(query)) return error("StorageBindingRequestInvalid", 400);
      const result = await prepared(env.EXECUTION_DB, query).run();
      return Response.json({ rows: result.results, changes: result.meta.changes });
    }
    if (url.pathname === "/batch" && request.method === "POST") {
      const value: unknown = await request.json();
      if (!Array.isArray(value) || value.length === 0 || value.length > 100 || !value.every(statement)) {
        return error("StorageBindingRequestInvalid", 400);
      }
      const results = await env.EXECUTION_DB.batch(value.map((query) => prepared(env.EXECUTION_DB!, query)));
      return Response.json(results.map((result) => ({ rows: result.results, changes: result.meta.changes })));
    }
    const blob = url.pathname.match(/^\/blob\/([a-f0-9]{64})$/);
    if (blob && DIGEST.test(blob[1])) {
      const key = `blobs/${blob[1]}`;
      if (request.method === "PUT") {
        if (!request.body) return error("StorageBindingRequestInvalid", 400);
        await env.EXECUTION_BLOBS.put(key, request.body);
        return new Response(null, { status: 204 });
      }
      if (request.method === "GET") {
        const object = await env.EXECUTION_BLOBS.get(key);
        return object ? new Response(object.body) : error("StorageBlobMissing", 404);
      }
      if (request.method === "DELETE") {
        await env.EXECUTION_BLOBS.delete(key);
        return new Response(null, { status: 204 });
      }
    }
    return error("StorageBindingRouteInvalid", 404);
  } catch {
    return error("StorageBindingFailed", 503);
  }
}
