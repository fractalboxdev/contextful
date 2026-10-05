export type StoreEntry = {
  id: string;
  label: string;
  endpoint: string;
  auth?: string;
  exchangeRoute?: string;
  packPrefix?: string;
  credentialName: string;
  bindingName: string;
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
  return {
    id: raw.id,
    label: typeof raw.label === "string" ? raw.label : raw.id,
    endpoint: endpoint.toString().replace(/\/$/, ""),
    auth: typeof raw.auth === "string" ? raw.auth : undefined,
    exchangeRoute: typeof raw.exchangeRoute === "string" ? raw.exchangeRoute : undefined,
    packPrefix: typeof raw.packPrefix === "string" ? raw.packPrefix : undefined,
    credentialName,
    bindingName,
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

function unavailable(): Response {
  return Response.json({ error: { identifier: "GatewayUnconfigured", message: "no stores are configured" } }, { status: 503, headers: { "Retry-After": "1" } });
}

export async function routeRequest(request: Request, registry: StoreRegistry, forward: Forward = async (_entry, proxied) => fetch(proxied)): Promise<Response> {
  if (registry.entries.length === 0) return unavailable();
  const url = new URL(request.url);
  const match = url.pathname.match(/^\/stores\/([^/]+)(\/.*)?$/);
  if (!match) return new Response("not found", { status: 404 });
  const entry = registry.entries.find((candidate) => candidate.id === match[1]);
  if (!entry) return new Response("store not found", { status: 404 });
  const suffix = match[2] ?? "/";
  const target = new URL(entry.endpoint);
  const basePath = target.pathname.replace(/\/$/, "");
  target.pathname = `${basePath}${suffix.startsWith("/") ? suffix : `/${suffix}`}`;
  target.search = url.search;
  const body = request.method === "GET" || request.method === "HEAD" ? undefined : await request.arrayBuffer();
  const proxied = new Request(target, { method: request.method, headers: request.headers, body });
  return forward(entry, proxied);
}

export default {
  async fetch(request: Request, env: { CONTEXTFUL_STORES_JSON?: string }): Promise<Response> {
    try {
      return await routeRequest(request, registryFromEnv(env.CONTEXTFUL_STORES_JSON));
    } catch (error) {
      if (error instanceof RegistryError) {
        const status = error.identifier === "StoreRegistryUnreadable" ? 503 : 500;
        return Response.json({ error: { identifier: error.identifier, message: error.message } }, { status });
      }
      return Response.json({ error: { identifier: "GatewayFailure", message: "gateway request failed" } }, { status: 500 });
    }
  },
};
