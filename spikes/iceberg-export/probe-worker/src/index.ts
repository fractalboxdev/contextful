// Latency probe for #734: a Worker that maps the caller's credential to one audience
// namespace and times Basin SQL queries against that namespace's exported tables.
//
// GET /probe?kind=point|range|groupby|join&table=<table>&n=<samples>&budget_ms=<ms>
//   Authorization: Bearer <audience credential>
// Runs up to n sequential Basin SQL requests, stopping early once budget_ms has elapsed,
// and returns each request's wall time measured inside the Worker with the metrics Basin
// SQL reports. The caller aggregates samples across calls.

export interface Env {
  /** Basin SQL API token: Basin SQL read, Basin Catalog read, R2 object read on one bucket. */
  BASIN_SQL_TOKEN: string;
  ACCOUNT_ID: string;
  BUCKET: string;
  /** JSON object: hex SHA-256 of an audience credential → that audience's namespace. */
  AUDIENCES: string;
}

const IDENT = /^[a-z_][a-z0-9_]{0,62}$/;
/** Rows per benchmark table; `point` draws its key from this range. */
const ROWS: Record<string, number> = { small: 100_000, medium: 10_000_000, wide_files: 10_000_000 };
const KINDS = ["point", "range", "groupby", "join"] as const;
type Kind = (typeof KINDS)[number];

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

/** The Worker's own query templates; caller input chooses a template and a table only. */
function query(kind: Kind, ns: string, table: string): string {
  const t = `${ns}.${table}`;
  switch (kind) {
    case "point": {
      const key = Math.floor(Math.random() * (ROWS[table] ?? 1));
      return `SELECT key, label, hits, amount, observed_at FROM ${t} WHERE key = ${key}`;
    }
    case "range":
      return `SELECT count(*) AS n, sum(hits) AS hits FROM ${t} WHERE observed_at >= '2026-01-10T00:00:00Z' AND observed_at < '2026-01-11T00:00:00Z' AND dim_id < 100`;
    case "groupby":
      return `SELECT label, count(*) AS n, sum(hits) AS hits FROM ${t} GROUP BY label`;
    case "join":
      return `SELECT d.region, count(*) AS n, sum(f.hits) AS hits FROM ${t} f JOIN ${ns}.dim d ON f.dim_id = d.dim_id GROUP BY d.region`;
  }
}

interface Sample {
  ms: number;
  status: number;
  metrics?: Record<string, number>;
  error?: string;
}

async function basinSql(env: Env, sql: string): Promise<Sample> {
  const url = `https://api.sql.cloudflarestorage.com/api/v1/accounts/${env.ACCOUNT_ID}/basin-sql/query/${env.BUCKET}`;
  const t0 = performance.now();
  const res = await fetch(url, {
    method: "POST",
    headers: { authorization: `Bearer ${env.BASIN_SQL_TOKEN}`, "content-type": "application/json" },
    body: JSON.stringify({ query: sql }),
  });
  const text = await res.text();
  // Workers advance the clock across I/O, so the difference spans request and body.
  const ms = performance.now() - t0;
  try {
    const body = JSON.parse(text) as { success?: boolean; result?: { metrics?: Record<string, number> }; errors?: unknown };
    if (res.status === 200 && body.success) return { ms, status: res.status, metrics: body.result?.metrics };
    return { ms, status: res.status, error: JSON.stringify(body.errors ?? body).slice(0, 400) };
  } catch {
    return { ms, status: res.status, error: text.slice(0, 400) };
  }
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    if (url.pathname !== "/probe") return new Response("not found", { status: 404 });
    const ns = await namespaceFor(req, env);
    if (!ns) return new Response("unknown credential", { status: 401 });
    const kind = (url.searchParams.get("kind") ?? "point") as Kind;
    if (!KINDS.includes(kind)) return new Response("bad kind", { status: 400 });
    const table = url.searchParams.get("table") ?? "small";
    if (!IDENT.test(table)) return new Response("bad table", { status: 400 });
    const n = Math.min(500, Math.max(1, Number(url.searchParams.get("n") ?? "10")));
    const budget = Number(url.searchParams.get("budget_ms") ?? "60000");

    const started = Date.now();
    const samples: Sample[] = [];
    while (samples.length < n && Date.now() - started < budget) {
      samples.push(await basinSql(env, query(kind, ns, table)));
    }
    return Response.json({ kind, table, colo: (req as Request & { cf?: { colo?: string } }).cf?.colo, samples });
  },
} satisfies ExportedHandler<Env>;
