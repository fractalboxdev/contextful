// Latency probe for #734: a Worker that maps the caller's credential to one audience
// namespace and times Basin SQL queries against that namespace's exported table.
//
// GET /probe?kind=point|agg&table=<table>&n=<samples>
//   Authorization: Bearer <audience credential>
// Returns p50/p99 wall time of n sequential Basin SQL requests after one warm-up request.

export interface Env {
  /** Basin SQL API token: Basin SQL read, Basin Catalog read, R2 storage read on one bucket. */
  BASIN_SQL_TOKEN: string;
  ACCOUNT_ID: string;
  BUCKET: string;
  /** JSON object: hex SHA-256 of an audience credential → that audience's namespace. */
  AUDIENCES: string;
}

const IDENT = /^[a-z_][a-z0-9_]{0,62}$/;
const ROWS = 1_000_000;

async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** The namespace the credential reads, or null. The caller never names a namespace. */
async function namespaceFor(req: Request, env: Env): Promise<string | null> {
  const auth = req.headers.get("authorization") ?? "";
  const credential = auth.startsWith("Bearer ") ? auth.slice(7) : "";
  if (!credential) return null;
  const audiences = JSON.parse(env.AUDIENCES) as Record<string, string>;
  const ns = audiences[await sha256Hex(credential)];
  return ns && IDENT.test(ns) ? ns : null;
}

function query(kind: string, ns: string, table: string): string {
  if (kind === "point") {
    const id = 1 + Math.floor(Math.random() * ROWS);
    return `SELECT id, hits, amount, label, observed_at FROM ${ns}.${table} WHERE id = ${id}`;
  }
  return `SELECT label, count(*) AS n, sum(hits) AS hits FROM ${ns}.${table} GROUP BY label`;
}

async function basinSql(env: Env, sql: string): Promise<{ ms: number; status: number; body: unknown }> {
  const url = `https://api.sql.cloudflarestorage.com/api/v1/accounts/${env.ACCOUNT_ID}/basin-sql/query/${env.BUCKET}`;
  const t0 = performance.now();
  const res = await fetch(url, {
    method: "POST",
    headers: { authorization: `Bearer ${env.BASIN_SQL_TOKEN}`, "content-type": "application/json" },
    body: JSON.stringify({ query: sql }),
  });
  const body = await res.json().catch(() => null);
  // Workers advance the clock across I/O, so the difference spans the request.
  return { ms: performance.now() - t0, status: res.status, body };
}

function percentile(sorted: number[], p: number): number {
  return sorted[Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1)];
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname !== "/probe") return new Response("not found", { status: 404 });
    const ns = await namespaceFor(req, env);
    if (!ns) return new Response("unknown credential", { status: 401 });
    const kind = url.searchParams.get("kind") === "agg" ? "agg" : "point";
    const table = url.searchParams.get("table") ?? "events_v2";
    if (!IDENT.test(table)) return new Response("bad table", { status: 400 });
    const n = Math.min(200, Math.max(1, Number(url.searchParams.get("n") ?? "30")));

    const warm = await basinSql(env, query(kind, ns, table));
    if (warm.status !== 200) return Response.json({ error: "warm-up failed", status: warm.status, body: warm.body }, { status: 502 });
    const samples: number[] = [];
    let failures = 0;
    for (let i = 0; i < n; i++) {
      const r = await basinSql(env, query(kind, ns, table));
      if (r.status === 200) samples.push(r.ms);
      else failures++;
    }
    const sorted = [...samples].sort((a, b) => a - b);
    return Response.json({
      kind,
      table,
      n: samples.length,
      failures,
      warm_ms: warm.ms,
      p50_ms: percentile(sorted, 50),
      p99_ms: percentile(sorted, 99),
      min_ms: sorted[0],
      max_ms: sorted[sorted.length - 1],
      colo: (req as Request & { cf?: { colo?: string } }).cf?.colo,
      sample_response: warm.body,
    });
  },
} satisfies ExportedHandler<Env>;
