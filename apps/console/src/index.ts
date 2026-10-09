import { createHash, createHmac, createPublicKey, randomBytes, timingSafeEqual, verify, type KeyObject } from "node:crypto";
import { readFileSync } from "node:fs";
import { deriveBrief, withinBriefBudget, type BriefInput } from "./brief.ts";
import { answerDelivery } from "./publish.ts";
import { buildView, sanitizeView, type ResultRows } from "./render.ts";
import { ConsoleError } from "./turn.ts";
import { parseVantage } from "./turn.ts";
import { BrowseError } from "./browse.ts";

export type Operator = { subject: string; grants: ReadonlySet<"query" | "admin">; assertion?: string; session?: string };
export type Store = { id: string; label: string };
export type AccessIdentity = {
  kind: "access";
  issuer: string;
  queryAudience: string;
  adminAudience: string;
  memorySessionClaim?: string;
  publicKey?: KeyObject | string;
  keys?: ReadonlyMap<string, KeyObject>;
};
export type CognitoIdentity = {
  kind: "cognito";
  sessionSecret: string;
  queryGroup: string;
  adminGroup: string;
  memorySessionClaim?: string;
  issuer?: string;
  clientId?: string;
  authorizeUrl?: string;
  tokenUrl?: string;
  redirectUri?: string;
  publicKey?: KeyObject | string;
  keys?: ReadonlyMap<string, KeyObject>;
  fetcher?: typeof fetch;
};
export type Identity = AccessIdentity | CognitoIdentity;
export type TurnInput = { operator: Operator; store: string; question: string };
export type TurnResult = { answer: string; sources: unknown[]; widgets: unknown[]; resultRows?: ResultRows;
  evidence?: Array<{ table: string; run: string; seq: number }>; accessExplanation?: boolean; share?: boolean };
export type PackList = { entries: unknown[]; truncated: boolean; declined: number };
export type ConsoleAdapters = {
  identity: Identity;
  stores: Store[];
  adminCapability?: string;
  turn: (input: TurnInput) => Promise<TurnResult>;
  brief?: (operator: Operator, store: string) => Promise<Omit<BriefInput, "budgetMs">>;
  briefBudgetMs?: number;
  redactView?: (operator: Operator, value: string) => string;
  read: { list: (operator: Operator) => Promise<Store[]> };
  browse?: {
    discover: (input: { operator: Operator; store: string; asOf?: string }) => Promise<unknown>;
    preview: (input: { operator: Operator; store: string; asOf?: string; path: string }) => Promise<unknown>;
  };
  control: {
    workflows: (operator: Operator, store: string | null) => Promise<unknown>;
    record: (operator: Operator, store: string | null) => Promise<unknown>;
    edit: (document: unknown, capability: string, operator: Operator) => Promise<unknown>;
    apply: (document: unknown, capability: string, operator: Operator) => Promise<unknown>;
    listPackFiles?: (operator: Operator, store: string, prefix: string) => Promise<PackList>;
  };
};

type Session = { subject: string; groups: string[]; expiresAt: number; assertion?: string; session?: string };

function encoded(value: unknown): string {
  return Buffer.from(JSON.stringify(value)).toString("base64url");
}

function decode(value: string): unknown {
  return JSON.parse(Buffer.from(value, "base64url").toString("utf8"));
}

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function secureEqual(left: Buffer, right: Buffer): boolean {
  return left.length === right.length && timingSafeEqual(left, right);
}

export function issueCognitoSession(session: Session, secret: string): string {
  if (!secret || !session.subject || !Number.isSafeInteger(session.expiresAt) || session.expiresAt <= Math.floor(Date.now() / 1000)) {
    throw new Error("CognitoSessionInvalid");
  }
  const body = encoded(session);
  const signature = createHmac("sha256", secret).update(body).digest("base64url");
  return `${body}.${signature}`;
}

function verifyCognitoSession(cookie: string, identity: CognitoIdentity): Operator | null {
  const token = cookie.split("; ").find((part) => part.startsWith("console_session="))?.slice("console_session=".length);
  if (!token) return null;
  const parts = token.split(".");
  if (parts.length !== 2) return null;
  const expected = createHmac("sha256", identity.sessionSecret).update(parts[0]).digest();
  let actual: Buffer;
  try { actual = Buffer.from(parts[1], "base64url"); } catch { return null; }
  if (!secureEqual(expected, actual)) return null;
  let session: unknown;
  try { session = decode(parts[0]); } catch { return null; }
  if (!object(session) || typeof session.subject !== "string" || !session.subject ||
      !Number.isSafeInteger(session.expiresAt) || (session.expiresAt as number) <= Math.floor(Date.now() / 1000) ||
      !Array.isArray(session.groups) || !session.groups.every((group) => typeof group === "string") ||
      (session.assertion !== undefined && typeof session.assertion !== "string") ||
      (session.session !== undefined && typeof session.session !== "string")) return null;
  const grants = new Set<"query" | "admin">();
  if (session.groups.includes(identity.queryGroup)) grants.add("query");
  if (session.groups.includes(identity.adminGroup)) grants.add("admin");
  return { subject: session.subject, grants, assertion: typeof session.assertion === "string" ? session.assertion : undefined,
    session: session.session };
}

function verifyAccess(assertion: string, identity: AccessIdentity): Operator | null {
  const parts = assertion.split(".");
  if (parts.length !== 3) return null;
  let header: unknown;
  let claims: unknown;
  try { header = decode(parts[0]); claims = decode(parts[1]); } catch { return null; }
  if (!object(header) || header.alg !== "RS256" || !object(claims)) return null;
  if (claims.iss !== identity.issuer || typeof claims.sub !== "string" || !claims.sub ||
      !Number.isSafeInteger(claims.exp) || (claims.exp as number) <= Math.floor(Date.now() / 1000)) return null;
  if (typeof claims.nbf === "number" && claims.nbf > Math.floor(Date.now() / 1000)) return null;
  const audience = typeof claims.aud === "string" ? [claims.aud] : Array.isArray(claims.aud) ? claims.aud : [];
  const grants = new Set<"query" | "admin">();
  if (audience.includes(identity.queryAudience)) grants.add("query");
  if (audience.includes(identity.adminAudience)) grants.add("admin");
  if (grants.size === 0) return null;
  let signature: Buffer;
  try { signature = Buffer.from(parts[2], "base64url"); } catch { return null; }
  const key = identity.keys ? (typeof header.kid === "string" ? identity.keys.get(header.kid) : undefined) :
    typeof identity.publicKey === "string" ? createPublicKey(identity.publicKey) : identity.publicKey;
  if (!key) return null;
  if (!verify("RSA-SHA256", Buffer.from(`${parts[0]}.${parts[1]}`), key, signature)) return null;
  const session = identity.memorySessionClaim ? claims[identity.memorySessionClaim] : undefined;
  return { subject: claims.sub, grants, assertion, session: typeof session === "string" && session.trim() ? session : undefined };
}

function operatorFor(request: Request, identity: Identity): Operator | null {
  if (identity.kind === "access") {
    const assertion = request.headers.get("cf-access-jwt-assertion");
    return assertion ? verifyAccess(assertion, identity) : null;
  }
  return verifyCognitoSession(request.headers.get("cookie") ?? "", identity);
}

function cognitoLogin(identity: CognitoIdentity): Response {
  if (!identity.authorizeUrl || !identity.clientId || !identity.redirectUri) return refusal("ConsoleLoginUnconfigured", 503);
  const state = randomBytes(24).toString("base64url");
  const verifier = randomBytes(32).toString("base64url");
  const issued = Math.floor(Date.now() / 1000).toString();
  const loginData = `${state}.${verifier}.${issued}`;
  const signature = createHmac("sha256", identity.sessionSecret).update(loginData).digest("base64url");
  const location = new URL(identity.authorizeUrl);
  location.searchParams.set("response_type", "code");
  location.searchParams.set("client_id", identity.clientId);
  location.searchParams.set("redirect_uri", identity.redirectUri);
  location.searchParams.set("scope", "openid email profile");
  location.searchParams.set("state", state);
  location.searchParams.set("code_challenge", createHash("sha256").update(verifier).digest("base64url"));
  location.searchParams.set("code_challenge_method", "S256");
  return new Response(null, { status: 302, headers: {
    Location: location.toString(),
    "Set-Cookie": `console_login_state=${loginData}.${signature}; HttpOnly; Secure; SameSite=Lax; Path=/auth/callback; Max-Age=300`,
    "Cache-Control": "no-store",
  } });
}

async function cognitoCallback(request: Request, identity: CognitoIdentity): Promise<Response> {
  if (!identity.tokenUrl || !identity.clientId || !identity.redirectUri || !identity.issuer || (!identity.publicKey && !identity.keys)) return refusal("ConsoleLoginUnconfigured", 503);
  const url = new URL(request.url);
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  const loginCookie = request.headers.get("cookie")?.split(/;\s*/).find((part) => part.startsWith("console_login_state="))?.slice("console_login_state=".length);
  const loginParts = loginCookie?.split(".");
  if (!code || !state || !loginParts || loginParts.length !== 4) return refusal("ConsolePageForbidden");
  const [cookieState, verifier, issued, signature] = loginParts;
  const expected = createHmac("sha256", identity.sessionSecret).update(`${cookieState}.${verifier}.${issued}`).digest("base64url");
  const age = Math.floor(Date.now() / 1000) - Number(issued);
  if (!secureEqual(Buffer.from(state), Buffer.from(cookieState)) ||
      !secureEqual(Buffer.from(signature), Buffer.from(expected)) || !Number.isInteger(age) || age < 0 || age > 300 ||
      !/^[A-Za-z0-9_-]{43}$/.test(verifier)) return refusal("ConsolePageForbidden");
  const exchange = await (identity.fetcher ?? fetch)(identity.tokenUrl, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "authorization_code", client_id: identity.clientId, redirect_uri: identity.redirectUri, code, code_verifier: verifier }),
  });
  if (!exchange.ok) return refusal("ConsolePageForbidden");
  const tokens: unknown = await exchange.json();
  if (!object(tokens) || typeof tokens.id_token !== "string") return refusal("ConsolePageForbidden");
  const parts = tokens.id_token.split(".");
  if (parts.length !== 3) return refusal("ConsolePageForbidden");
  let header: unknown;
  let claims: unknown;
  try { header = decode(parts[0]); claims = decode(parts[1]); } catch { return refusal("ConsolePageForbidden"); }
  if (!object(header) || header.alg !== "RS256" || !object(claims) || claims.iss !== identity.issuer || claims.aud !== identity.clientId ||
      typeof claims.sub !== "string" || !claims.sub || !Number.isSafeInteger(claims.exp) || (claims.exp as number) <= Math.floor(Date.now() / 1000) ||
      claims.token_use !== "id") return refusal("ConsolePageForbidden");
  const key = identity.keys ? (typeof header.kid === "string" ? identity.keys.get(header.kid) : undefined) :
    typeof identity.publicKey === "string" ? createPublicKey(identity.publicKey) : identity.publicKey;
  if (!key) return refusal("ConsolePageForbidden");
  if (!verify("RSA-SHA256", Buffer.from(`${parts[0]}.${parts[1]}`), key, Buffer.from(parts[2], "base64url"))) return refusal("ConsolePageForbidden");
  const groups = Array.isArray(claims["cognito:groups"]) ? claims["cognito:groups"].filter((group): group is string => typeof group === "string") : [];
  const lifetime = Math.min(claims.exp as number, Math.floor(Date.now() / 1000) + 3600);
  const task = identity.memorySessionClaim ? claims[identity.memorySessionClaim] : undefined;
  const session = issueCognitoSession({ subject: claims.sub, groups, expiresAt: lifetime, assertion: tokens.id_token,
    session: typeof task === "string" && task.trim() ? task : undefined }, identity.sessionSecret);
  const destination = groups.includes(identity.adminGroup) ? "/admin" : "/query";
  return new Response(null, { status: 302, headers: {
    Location: destination,
    "Set-Cookie": `console_session=${session}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=${lifetime - Math.floor(Date.now() / 1000)}`,
    "Cache-Control": "no-store",
  } });
}

function refusal(identifier: string, status = 403): Response {
  return Response.json({ error: { identifier } }, { status, headers: { "Cache-Control": "no-store" } });
}

// An opaque digest of the verified operator and reading session; the Query page restores
// saved chats only under the digest that wrote them.
function transcriptScope(operator: Operator): string {
  return createHash("sha256").update(JSON.stringify([operator.subject, operator.session ?? null])).digest("base64url");
}

function json(value: unknown): Response {
  return Response.json(value, { headers: { "Cache-Control": "no-store" } });
}

async function body(request: Request): Promise<unknown> {
  const limit = 1_048_576;
  if (Number(request.headers.get("content-length") ?? 0) > limit) throw new Error("ConsoleBodyTooLarge");
  const chunks: Buffer[] = [];
  let size = 0;
  const reader = request.body?.getReader();
  if (!reader) return JSON.parse("");
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) {
        void reader.cancel().catch(() => {});
        throw new Error("ConsoleBodyTooLarge");
      }
      chunks.push(Buffer.from(value));
    }
  } finally {
    reader.releaseLock();
  }
  return JSON.parse(Buffer.concat(chunks, size).toString("utf8"));
}

function bodyFailure(error: unknown): Response {
  return error instanceof Error && error.message === "ConsoleBodyTooLarge" ?
    refusal("ConsoleBodyTooLarge", 413) : refusal("ConsoleRequestMalformed", 400);
}

const documents = new Map<"query" | "admin", string>();

// The `build:client` script writes each page as one self-contained document beside this
// module's package; the same relative path resolves from src/ and from the dist/ bundle.
function pageDocument(name: "query" | "admin"): string {
  let html = documents.get(name);
  if (html === undefined) {
    html = readFileSync(new URL(`../client/dist/${name}.html`, import.meta.url), "utf8");
    documents.set(name, html);
  }
  return html;
}

function page(name: "query" | "admin"): Response {
  return new Response(pageDocument(name), {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-store",
      "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; font-src data:; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
      "X-Content-Type-Options": "nosniff",
    },
  });
}

export function createConsole(adapters: ConsoleAdapters): { fetch: (request: Request) => Promise<Response> } {
  return {
    async fetch(request) {
      const url = new URL(request.url);
      const path = url.pathname;
      if (adapters.identity.kind === "cognito") {
        if (path === "/auth/login" && request.method === "GET") return cognitoLogin(adapters.identity);
        if (path === "/auth/callback" && request.method === "GET") return cognitoCallback(request, adapters.identity);
      }
      const grant = path === "/query" || path.startsWith("/query/api/") ? "query" :
        path === "/admin" || path.startsWith("/admin/api/") ? "admin" : null;
      if (!grant) return refusal("ConsoleRouteNotFound", 404);
      let operator: Operator | null;
      try { operator = operatorFor(request, adapters.identity); } catch { operator = null; }
      if (!operator) return refusal("ConsolePageForbidden", 401);
      if (!operator.grants.has(grant)) return refusal("ConsolePageForbidden");
      if (request.method === "POST" && request.headers.get("origin") !== url.origin) {
        return refusal("ConsolePageForbidden");
      }
      if (request.method === "GET" && path === `/${grant}`) return page(grant);
      if (grant === "query") {
        if (request.method === "GET" && path === "/query/api/stores") {
          const response = json(await adapters.read.list(operator));
          response.headers.set("X-Console-Transcript-Scope", transcriptScope(operator));
          return response;
        }
        if (request.method === "POST" && (path === "/query/api/browse" || path === "/query/api/preview")) {
          let input: unknown;
          try { input = await body(request); } catch (error) { return bodyFailure(error); }
          if (!object(input) || typeof input.store !== "string" || !adapters.stores.some((store) => store.id === input.store) ||
              (input.asOf !== undefined && typeof input.asOf !== "string") ||
              (path.endsWith("/preview") && (typeof input.path !== "string" || !input.path || input.path.length > 4096))) {
            return refusal("ConsoleRequestMalformed", 400);
          }
          let asOf: string | undefined;
          try { asOf = input.asOf === undefined ? undefined : parseVantage(input.asOf as string); }
          catch { return refusal("ConsoleVantageUnparseable", 400); }
          if (!adapters.browse) return refusal("ConsoleAdapterUnavailable", 503);
          try {
            return json(path.endsWith("/preview") ?
              await adapters.browse.preview({ operator, store: input.store, asOf, path: input.path as string }) :
              await adapters.browse.discover({ operator, store: input.store, asOf }));
          } catch (error) {
            if (error instanceof BrowseError) return refusal(error.code, error.code === "ConsoleGalleryPathUnlisted" ? 403 : 400);
            if (error instanceof ConsoleError) return refusal(error.code, error.status);
            throw error;
          }
        }
        if (request.method === "GET" && path === "/query/api/brief") {
          const store = url.searchParams.get("store");
          if (!store || !adapters.stores.some((entry) => entry.id === store)) return refusal("ConsoleRequestMalformed", 400);
          const brief = adapters.brief;
          if (!brief) return refusal("ConsoleAdapterUnavailable", 503);
          const budgetMs = adapters.briefBudgetMs ?? NaN;
          try {
            const card = await withinBriefBudget(budgetMs, async () => {
              const input = await brief(operator, store);
              return deriveBrief({ ...input, budgetMs });
            });
            return card ? json(card) : new Response(null, { status: 204, headers: { "Cache-Control": "no-store" } });
          } catch {
            return new Response(null, { status: 204, headers: { "Cache-Control": "no-store" } });
          }
        }
        if (request.method === "POST" && path === "/query/api/ask") {
          let input: unknown;
          try { input = await body(request); } catch (error) { return bodyFailure(error); }
          if (!object(input) || typeof input.store !== "string" || typeof input.question !== "string" || !input.question.trim() ||
              !adapters.stores.some((store) => store.id === input.store)) return refusal("ConsoleRequestMalformed", 400);
          if ("view" in input || "widgets" in input) return refusal("ConsoleViewNotServerBuilt", 400);
          let turn: TurnResult;
          try { turn = await adapters.turn({ operator, store: input.store, question: input.question }); }
          catch (error) {
            if (error instanceof ConsoleError) return refusal(error.code, error.status);
            throw error;
          }
          let answer: ReturnType<typeof answerDelivery>;
          try {
            answer = answerDelivery({ operator: operator.subject, answer: turn.answer, accessExplanation: turn.accessExplanation ?? false, share: turn.share });
          } catch (error) {
            return refusal(error instanceof Error ? error.message : "VisibilityShareAffordance");
          }
          const redactView = adapters.redactView;
          const clean = turn.resultRows && redactView ?
            sanitizeView({ component: "table.v1", props: turn.resultRows }, (value) => redactView(operator, value)) : null;
          const built = clean ? buildView(clean.props) : null;
          return json({ answer: answer.answer, sources: turn.sources, widgets: built ? [built] : [] });
        }
      } else {
        const store = url.searchParams.get("store");
        if (store !== null && !adapters.stores.some((entry) => entry.id === store)) return refusal("ConsoleRequestMalformed", 400);
        if (request.method === "GET" && path === "/admin/api/stores") return json(adapters.stores);
        if (request.method === "GET" && path === "/admin/api/workflows") return json(await adapters.control.workflows(operator, store));
        if (request.method === "GET" && path === "/admin/api/record") return json(await adapters.control.record(operator, store));
        if (request.method === "GET" && path === "/admin/api/packs") {
          if (!store || !adapters.stores.some((entry) => entry.id === store)) return refusal("ConsoleRequestMalformed", 400);
          if (!adapters.control.listPackFiles) return refusal("ConsoleAdapterUnavailable", 503);
          return json(await adapters.control.listPackFiles(operator, store, url.searchParams.get("prefix") ?? ""));
        }
        if (request.method === "POST" && (path === "/admin/api/edit" || path === "/admin/api/apply")) {
          if (!adapters.adminCapability) return refusal("ConsoleAdminGrantMissing");
          const origin = request.headers.get("origin");
          if (origin && origin !== url.origin) return refusal("ConsolePageForbidden");
          let document: unknown;
          try { document = await body(request); } catch (error) { return bodyFailure(error); }
          return json(path.endsWith("/edit") ?
            await adapters.control.edit(document, adapters.adminCapability, operator) :
            await adapters.control.apply(document, adapters.adminCapability, operator));
        }
      }
      return refusal("ConsoleRouteNotFound", 404);
    },
  };
}
