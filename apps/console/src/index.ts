import { createHash, createHmac, createPublicKey, randomBytes, timingSafeEqual, verify, type KeyObject } from "node:crypto";

export type Operator = { subject: string; grants: ReadonlySet<"query" | "admin"> };
export type Store = { id: string; label: string };
export type AccessIdentity = {
  kind: "access";
  issuer: string;
  queryAudience: string;
  adminAudience: string;
  publicKey?: KeyObject | string;
  keys?: ReadonlyMap<string, KeyObject>;
};
export type CognitoIdentity = {
  kind: "cognito";
  sessionSecret: string;
  queryGroup: string;
  adminGroup: string;
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
export type TurnResult = { answer: string; sources: unknown[]; widgets: unknown[] };
export type PackList = { entries: unknown[]; truncated: boolean; declined: number };
export type ConsoleAdapters = {
  identity: Identity;
  stores: Store[];
  adminCapability?: string;
  turn: (input: TurnInput) => Promise<TurnResult>;
  read: { list: (operator: Operator) => Promise<Store[]> };
  control: {
    workflows: (operator: Operator, store: string | null) => Promise<unknown>;
    record: (operator: Operator, store: string | null) => Promise<unknown>;
    edit: (document: unknown, capability: string, operator: Operator) => Promise<unknown>;
    apply: (document: unknown, capability: string, operator: Operator) => Promise<unknown>;
    listPackFiles?: (operator: Operator, store: string, prefix: string) => Promise<PackList>;
  };
};

type Session = { subject: string; groups: string[]; expiresAt: number };

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
      !Array.isArray(session.groups) || !session.groups.every((group) => typeof group === "string")) return null;
  const grants = new Set<"query" | "admin">();
  if (session.groups.includes(identity.queryGroup)) grants.add("query");
  if (session.groups.includes(identity.adminGroup)) grants.add("admin");
  return { subject: session.subject, grants };
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
  return { subject: claims.sub, grants };
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
  const session = issueCognitoSession({ subject: claims.sub, groups, expiresAt: lifetime }, identity.sessionSecret);
  const destination = groups.includes(identity.adminGroup) ? "/admin" : "/query";
  return new Response(null, { status: 302, headers: {
    Location: destination,
    "Set-Cookie": `console_session=${session}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=${lifetime - Math.floor(Date.now() / 1000)}`,
    "Cache-Control": "no-store",
  } });
}

function refusal(identifier: string, status = 403): Response {
  return Response.json({ error: { identifier } }, { status, headers: { "Cache-Control": "no-store" } });
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

function page(name: "query" | "admin"): Response {
  return new Response(name === "query" ? queryPage : adminPage, {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-store",
      "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
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
        if (request.method === "GET" && path === "/query/api/stores") return json(await adapters.read.list(operator));
        if (request.method === "POST" && path === "/query/api/ask") {
          let input: unknown;
          try { input = await body(request); } catch (error) { return bodyFailure(error); }
          if (!object(input) || typeof input.store !== "string" || typeof input.question !== "string" || !input.question.trim() ||
              !adapters.stores.some((store) => store.id === input.store)) return refusal("ConsoleRequestMalformed", 400);
          return json(await adapters.turn({ operator, store: input.store, question: input.question }));
        }
      } else {
        const store = url.searchParams.get("store");
        if (store !== null && !adapters.stores.some((entry) => entry.id === store)) return refusal("ConsoleRequestMalformed", 400);
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

const sharedStyle = `<style>
:root{font-family:ui-sans-serif,system-ui,sans-serif;color:#182b39;background:#e7edf0}*{box-sizing:border-box}body{margin:0;min-height:100vh}.shell{display:grid;grid-template-columns:210px minmax(0,1fr);min-height:100vh}nav{background:#173b50;color:#eaf4f5;padding:26px 20px}nav strong{font-size:1.3rem;letter-spacing:-.04em}nav a{display:block;color:#eaf4f5;text-decoration:none;margin-top:24px;padding:9px 11px;border-radius:7px}nav a[aria-current]{background:#31657b}main{padding:30px min(5vw,64px);max-width:1200px;width:100%}h1{font-size:clamp(2rem,4vw,3rem);letter-spacing:-.055em;margin:5px 0 12px}h2{font-size:1.1rem}p{line-height:1.5}section{background:#fff;border:1px solid #c8d5da;border-radius:12px;padding:20px;margin:18px 0}button,select,textarea{font:inherit}button{background:#125a72;color:white;border:0;border-radius:7px;padding:10px 16px;cursor:pointer}button:focus-visible,a:focus-visible,select:focus-visible,textarea:focus-visible{outline:3px solid #efae4c;outline-offset:2px}textarea{width:100%;min-height:100px;padding:12px;border:1px solid #91a8b2;border-radius:7px}select{padding:8px;border:1px solid #91a8b2;border-radius:7px}pre{white-space:pre-wrap;overflow-wrap:anywhere}.muted{color:#536b78}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:14px}.node{border:1px solid #afc5cf;border-left:5px solid #3c7c92;border-radius:7px;padding:13px;background:#f5f9fa}#transcript article{border-left:3px solid #3c7c92;padding:8px 16px;margin:12px 0}#widgets table{border-collapse:collapse;width:100%}#widgets td,#widgets th{border-bottom:1px solid #c8d5da;padding:8px;text-align:left}@media(max-width:650px){.shell{display:block}nav{display:flex;gap:12px;align-items:center;padding:12px 20px}nav a{margin:0}main{padding:20px}}@media(prefers-reduced-motion:reduce){*{scroll-behavior:auto!important}}
</style>`;

const queryPage = `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Query · Contextful</title>${sharedStyle}<div class="shell"><nav aria-label="Console"><strong>Contextful</strong><a href="/query" aria-current="page">Query</a><a href="/admin">Admin</a></nav><main><h1>Ask the store</h1><p class="muted">Answers cite the rows your access permits.</p><section><form id="composer"><label for="store">Store</label> <select id="store" required></select><p><label for="question">Question</label></p><textarea id="question" required></textarea><p><button type="submit">Ask question</button></p></form></section><section><h2>Conversation</h2><div id="transcript" role="log" aria-live="polite"><p class="muted">Your answer appears here.</p></div></section><section><h2>Results</h2><div id="widgets"></div></section></main></div><script>
const form=document.getElementById('composer'),store=document.getElementById('store'),transcript=document.getElementById('transcript'),widgets=document.getElementById('widgets');
fetch('/query/api/stores').then(r=>r.json()).then(rows=>{for(const row of rows){const option=document.createElement('option');option.value=row.id;option.textContent=row.label;store.append(option)}});
function draw(widget){const props=widget.props??{};if(widget.component==='table.v1'&&Array.isArray(props.columns)&&Array.isArray(props.rows)){const table=document.createElement('table'),head=document.createElement('thead'),header=document.createElement('tr'),body=document.createElement('tbody');for(const column of props.columns){const cell=document.createElement('th');cell.textContent=String(column);header.append(cell)}head.append(header);for(const row of props.rows){const line=document.createElement('tr');for(const value of row){const cell=document.createElement('td');cell.textContent=String(value??'');line.append(cell)}body.append(line)}table.append(head,body);return table}if(widget.component==='metric.v1'){const metric=document.createElement('p');metric.style.fontSize='2rem';metric.textContent=String(props.value??'');return metric}const fallback=document.createElement('pre');fallback.textContent=JSON.stringify(props,null,2);return fallback}
form.addEventListener('submit',async event=>{event.preventDefault();const question=document.getElementById('question').value.trim();if(!question)return;const response=await fetch('/query/api/ask',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({store:store.value,question})});const answer=await response.json();transcript.replaceChildren();const item=document.createElement('article');item.textContent=question+'\n'+(answer.answer??answer.error?.identifier??'No answer');transcript.append(item);widgets.replaceChildren();for(const widget of answer.widgets??[])widgets.append(draw(widget));if(answer.sources?.length){const sources=document.createElement('p');sources.textContent='Sources: '+answer.sources.map(x=>x.title??x.url).join(', ');transcript.append(sources)}});
</script></html>`;

const adminPage = `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Admin · Contextful</title>${sharedStyle}<div class="shell"><nav aria-label="Console"><strong>Contextful</strong><a href="/query">Query</a><a href="/admin" aria-current="page">Admin</a></nav><main><h1>Store operations</h1><p class="muted">Pipelines, schedules, steps and run outcomes come from the store.</p><section><h2>Workflow canvas</h2><div id="canvas" class="grid"></div></section><section><h2>Operational record</h2><div id="record"></div></section><section><h2>Control document</h2><label for="document">Document</label><textarea id="document"></textarea><p><button id="edit">Edit</button> <button id="apply">Apply</button></p><p id="result" role="status"></p></section></main></div><script>
const canvas=document.getElementById('canvas'),record=document.getElementById('record');fetch('/admin/api/workflows').then(r=>r.json()).then(data=>{for(const pipeline of data.pipelines??[]){const node=document.createElement('article');node.className='node';node.textContent=[pipeline.id,pipeline.schedule,...(pipeline.steps??[]),...(pipeline.runs??[]).map(run=>run.status)].filter(Boolean).join(' · ');canvas.append(node)}if(!canvas.children.length)canvas.textContent='No workflows are published.'});fetch('/admin/api/record').then(r=>r.json()).then(data=>{record.textContent=JSON.stringify(data,null,2)});for(const action of ['edit','apply'])document.getElementById(action).addEventListener('click',async()=>{const text=document.getElementById('document').value;let documentValue;try{documentValue=JSON.parse(text)}catch{document.getElementById('result').textContent='Enter a valid JSON document.';return}const response=await fetch('/admin/api/'+action,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(documentValue)});document.getElementById('result').textContent=response.ok?action+' complete':(await response.json()).error?.identifier??'Request failed'});
</script></html>`;
