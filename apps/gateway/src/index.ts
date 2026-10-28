/** A catalog query engine reading the store's Iceberg exports (`run.export-iceberg.route`). */
export type BasinEntry = {
  endpoint: string;
  /** Operator SQL with `{namespace}` and `{table}` placeholders, by name. */
  templates: Record<string, string>;
  /** Whether the bucket holds one audience, so caller-written SQL may reach it (`run.export-iceberg.caller-sql`). */
  callerSql: boolean;
  /** The Worker secret holding the bucket's read-only Basin SQL token. */
  tokenName: string;
  /** The Worker secret mapping each audience key's SHA-256 to its one namespace. */
  audiencesName: string;
};

export type StoreEntry = {
  id: string;
  label: string;
  endpoint: string;
  auth?: string;
  exchangeRoute?: string;
  packPrefix?: string;
  credentialName: string;
  bindingName: string;
  basin?: BasinEntry;
};

export type RegistryProblem = {
  identifier: "StoreEntryMalformed";
  entry: number;
};

export type RegistryErrorIdentifier = "StoreRegistryUnreadable" | "StoreIdReserved" | "StoreNameAuthored";

export class RegistryError extends Error {
  readonly identifier: RegistryErrorIdentifier;

  constructor(
    identifier: RegistryErrorIdentifier,
    message: string,
  ) {
    super(`${identifier}: ${message}`);
    this.identifier = identifier;
    this.name = identifier;
  }
}

export type StoreRegistry = {
  entries: StoreEntry[];
  problems: RegistryProblem[];
};

const RESERVED_IDS = new Set(["admin", "query"]);
const ID = /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/;

function names(id: string): { credentialName: string; bindingName: string } {
  const bindingName = id.replaceAll("-", "_").toUpperCase();
  return { bindingName, credentialName: `${bindingName}_QUERY_TOKEN` };
}

function decodeBasin(value: unknown, index: number, bindingName: string): BasinEntry {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw malformed(index);
  const raw = value as Record<string, unknown>;
  for (const key of ["tokenName", "audiencesName", "token", "audiences"]) {
    if (key in raw) throw new RegistryError("StoreNameAuthored", `basin.${key}`);
  }
  let endpoint: URL;
  try {
    endpoint = new URL(String(raw.endpoint));
  } catch {
    throw malformed(index);
  }
  if (endpoint.protocol !== "https:" && endpoint.protocol !== "http:") throw malformed(index);
  const templates = raw.templates ?? {};
  if (!templates || typeof templates !== "object" || Array.isArray(templates) || Object.values(templates).some((t) => typeof t !== "string")) throw malformed(index);
  if ("caller_sql" in raw && typeof raw.caller_sql !== "boolean") throw malformed(index);
  return {
    endpoint: endpoint.toString(),
    templates: templates as Record<string, string>,
    callerSql: raw.caller_sql === true,
    tokenName: `${bindingName}_BASIN_SQL_TOKEN`,
    audiencesName: `${bindingName}_BASIN_AUDIENCES`,
  };
}

function malformed(index: number): RegistryProblem {
  return { identifier: "StoreEntryMalformed", entry: index };
}

function decodeEntry(value: unknown, index: number): StoreEntry {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw malformed(index);
  const raw = value as Record<string, unknown>;
  if (typeof raw.id !== "string" || !ID.test(raw.id)) throw malformed(index);
  if (RESERVED_IDS.has(raw.id)) throw new RegistryError("StoreIdReserved", raw.id);
  for (const key of ["credential", "credentialName", "binding", "bindingName"]) {
    if (key in raw) throw new RegistryError("StoreNameAuthored", key);
  }
  if (typeof raw.endpoint !== "string") throw malformed(index);
  let endpoint: URL;
  try {
    endpoint = new URL(raw.endpoint);
  } catch {
    throw malformed(index);
  }
  if (endpoint.protocol !== "http:" && endpoint.protocol !== "https:") throw malformed(index);
  for (const key of ["label", "auth", "exchangeRoute", "packPrefix"]) {
    if (key in raw && typeof raw[key] !== "string") throw malformed(index);
  }
  const { credentialName, bindingName } = names(raw.id);
  const basin = "basin" in raw ? decodeBasin(raw.basin, index, bindingName) : undefined;
  return {
    id: raw.id,
    label: typeof raw.label === "string" ? raw.label : raw.id,
    endpoint: endpoint.toString().replace(/\/$/, ""),
    auth: typeof raw.auth === "string" ? raw.auth : undefined,
    exchangeRoute: typeof raw.exchangeRoute === "string" ? raw.exchangeRoute : undefined,
    packPrefix: typeof raw.packPrefix === "string" ? raw.packPrefix : undefined,
    credentialName,
    bindingName,
    basin,
  };
}

export function registryFromEnv(raw: string | undefined): StoreRegistry {
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw ?? "[]");
  } catch (error) {
    throw new RegistryError("StoreRegistryUnreadable", error instanceof Error ? error.message : "invalid JSON");
  }
  if (!Array.isArray(parsed)) throw new RegistryError("StoreRegistryUnreadable", "expected an array");
  const entries: StoreEntry[] = [];
  const problems: RegistryProblem[] = [];
  for (const [index, value] of parsed.entries()) {
    try {
      entries.push(decodeEntry(value, index));
    } catch (error) {
      if (error instanceof RegistryError && (error.identifier === "StoreIdReserved" || error.identifier === "StoreNameAuthored")) throw error;
      problems.push(malformed(index));
    }
  }
  return { entries, problems };
}

type Forward = (entry: StoreEntry, request: Request) => Promise<Response>;

/** The Worker secrets and the Basin SQL client a route reads through. */
export type RouteContext = {
  secret: (name: string) => string | undefined;
  basin: (url: string, init: RequestInit) => Promise<Response>;
};

const NO_CONTEXT: RouteContext = { secret: () => undefined, basin: (url, init) => fetch(url, init) };
const IDENT = /^[a-z_][a-z0-9_]{0,127}$/;

function refused(status: number, message: string): Response {
  return Response.json({ error: { identifier: "ExportQueryRefused", message } }, { status });
}

async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/** The namespace the caller's audience key maps to, or `undefined` (`run.export-iceberg.audience`). */
async function audienceOf(request: Request, basin: BasinEntry, context: RouteContext): Promise<string | undefined> {
  const auth = request.headers.get("authorization") ?? "";
  if (!auth.startsWith("Bearer ")) return undefined;
  let audiences: Record<string, unknown>;
  try {
    audiences = JSON.parse(context.secret(basin.audiencesName) ?? "{}") as Record<string, unknown>;
  } catch {
    return undefined;
  }
  const namespace = audiences[await sha256Hex(auth.slice(7))];
  return typeof namespace === "string" && IDENT.test(namespace) ? namespace : undefined;
}

/** Answer a mapped caller's `POST /query` from Basin SQL: a template against its namespace, or its own SQL on a single-audience bucket (`run.export-iceberg.caller-sql`). */
async function basinQuery(request: Request, suffix: string, basin: BasinEntry, namespace: string, context: RouteContext): Promise<Response> {
  if (suffix !== "/query" || request.method !== "POST") return new Response("an export audience reads through POST /query alone", { status: 404 });
  let body: Record<string, unknown>;
  try {
    body = (await request.json()) as Record<string, unknown>;
  } catch {
    return refused(400, "the body is a JSON object");
  }
  if (!body || typeof body !== "object" || Array.isArray(body)) return refused(400, "the body is a JSON object");
  if ("namespace" in body) return refused(400, "the namespace follows from the audience key; a request names none");
  let sql: string;
  if (typeof body.sql === "string") {
    if (!basin.callerSql) return refused(403, "caller-written SQL needs a single-audience bucket; name a template");
    sql = body.sql;
  } else {
    const template = typeof body.template === "string" ? basin.templates[body.template] : undefined;
    if (template === undefined) return refused(400, "name a declared template");
    if (typeof body.table !== "string" || !IDENT.test(body.table)) return refused(400, "`table` is a lowercase identifier");
    sql = template.replaceAll("{namespace}", namespace).replaceAll("{table}", body.table);
  }
  const token = context.secret(basin.tokenName);
  if (!token) return Response.json({ error: { identifier: "GatewayUnconfigured", message: `${basin.tokenName} is unset` } }, { status: 503 });
  return context.basin(basin.endpoint, { method: "POST", headers: { authorization: `Bearer ${token}`, "content-type": "application/json" }, body: JSON.stringify({ query: sql }) });
}

function unavailable(): Response {
  return Response.json({ error: { identifier: "GatewayUnconfigured", message: "no stores are configured" } }, { status: 503, headers: { "Retry-After": "1" } });
}

export async function routeRequest(request: Request, registry: StoreRegistry, forward: Forward = async (_entry, proxied) => fetch(proxied), context: RouteContext = NO_CONTEXT): Promise<Response> {
  if (registry.entries.length === 0) return unavailable();
  const url = new URL(request.url);
  const match = url.pathname.match(/^\/stores\/([^/]+)(\/.*)?$/);
  if (!match) return new Response("not found", { status: 404 });
  const entry = registry.entries.find((candidate) => candidate.id === match[1]);
  if (!entry) return new Response("store not found", { status: 404 });
  const suffix = match[2] ?? "/";
  // A caller whose audience key maps to a namespace reads Basin SQL; every other caller the Container (`run.export-iceberg.route`).
  if (entry.basin) {
    const namespace = await audienceOf(request, entry.basin, context);
    if (namespace) return basinQuery(request, suffix, entry.basin, namespace, context);
  }
  const target = new URL(entry.endpoint);
  const basePath = target.pathname.replace(/\/$/, "");
  target.pathname = `${basePath}${suffix.startsWith("/") ? suffix : `/${suffix}`}`;
  target.search = url.search;
  const body = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer();
  const proxied = new Request(target, { method: request.method, headers: request.headers, body });
  return forward(entry, proxied);
}

export default {
  async fetch(request: Request, env: { CONTEXTFUL_STORES_JSON?: string } & Record<string, unknown>): Promise<Response> {
    try {
      const context: RouteContext = { secret: (name) => (typeof env[name] === "string" ? (env[name] as string) : undefined), basin: (url, init) => fetch(url, init) };
      return await routeRequest(request, registryFromEnv(env.CONTEXTFUL_STORES_JSON), undefined, context);
    } catch (error) {
      if (error instanceof RegistryError) {
        const status = error.identifier === "StoreRegistryUnreadable" ? 503 : 500;
        return Response.json({ error: { identifier: error.identifier, message: error.message } }, { status });
      }
      return Response.json({ error: { identifier: "GatewayFailure", message: "gateway request failed" } }, { status: 500 });
    }
  },
};
