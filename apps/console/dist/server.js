// src/entry.ts
import { createPublicKey as createPublicKey2 } from "node:crypto";
import { pathToFileURL } from "node:url";
import { isAbsolute, resolve } from "node:path";

// ../gateway/src/index.ts
var RegistryError = class extends Error {
  identifier;
  constructor(identifier, message) {
    super(`${identifier}: ${message}`);
    this.identifier = identifier;
    this.name = identifier;
  }
};
var RESERVED_IDS = /* @__PURE__ */ new Set(["admin", "query"]);
var ID = /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/;
function names(id) {
  const bindingName = id.replaceAll("-", "_").toUpperCase();
  return { bindingName, credentialName: `${bindingName}_QUERY_TOKEN` };
}
function malformed(index) {
  return { identifier: "StoreEntryMalformed", entry: index };
}
function decodeEntry(value, index) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw malformed(index);
  const raw = value;
  if (typeof raw.id !== "string" || !ID.test(raw.id)) throw malformed(index);
  if (RESERVED_IDS.has(raw.id)) throw new RegistryError("StoreIdReserved", raw.id);
  for (const key of ["credential", "credentialName", "binding", "bindingName"]) {
    if (key in raw) throw new RegistryError("StoreNameAuthored", key);
  }
  if (typeof raw.endpoint !== "string") throw malformed(index);
  let endpoint;
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
    auth: typeof raw.auth === "string" ? raw.auth : void 0,
    exchangeRoute: typeof raw.exchangeRoute === "string" ? raw.exchangeRoute : void 0,
    packPrefix: typeof raw.packPrefix === "string" ? raw.packPrefix : void 0,
    credentialName,
    bindingName
  };
}
function registryFromEnv(raw) {
  let parsed;
  try {
    parsed = JSON.parse(raw ?? "[]");
  } catch (error) {
    throw new RegistryError("StoreRegistryUnreadable", error instanceof Error ? error.message : "invalid JSON");
  }
  if (!Array.isArray(parsed)) throw new RegistryError("StoreRegistryUnreadable", "expected an array");
  const entries = [];
  const problems = [];
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

// src/turn.ts
var ConsoleError = class extends Error {
  code;
  status;
  constructor(code, message = code, status = 400) {
    super(message);
    this.name = "ConsoleError";
    this.code = code;
    this.status = status;
  }
};
function validatePack(pack) {
  if (pack.face === "organization") {
    const write = pack.tools.find((tool) => tool.kind === "write");
    if (write) throw new ConsoleError("ConsoleWriteToolOnOrgFace", `${pack.name}: ${write.name}`);
  }
}
function parseVantage(value) {
  const year = Number(value.slice(0, 4));
  const month = Number(value.slice(5, 7));
  const day = Number(value.slice(8, 10));
  const validDay = Number.isInteger(year) && Number.isInteger(month) && Number.isInteger(day) && new Date(Date.UTC(year, month - 1, day)).toISOString().slice(0, 10) === value.slice(0, 10);
  if (/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    const date = /* @__PURE__ */ new Date(`${value}T00:00:00Z`);
    if (validDay && !Number.isNaN(date.getTime()) && date.toISOString().slice(0, 10) === value) return value;
  } else if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) {
    const date = new Date(value);
    if (validDay && !Number.isNaN(date.getTime())) return value;
  }
  throw new ConsoleError("ConsoleVantageUnparseable", value, 400);
}
function parseWebBound(value) {
  try {
    return parseVantage(value);
  } catch {
    throw new ConsoleError("ConsoleWebBoundUnparseable", value);
  }
}
async function resolveReaderCredential(options) {
  try {
    const credential = await options.mint();
    if (credential.trim()) return credential;
    throw new ConsoleError("ConsoleTokenExchangeUnavailable", "exchange returned an empty credential", 503);
  } catch (error) {
    if (!(error instanceof ConsoleError && error.code === "ConsoleTokenExchangeRefused" && error.status === 403)) throw error;
  }
  if (options.shared) return options.shared;
  throw new ConsoleError("ConsoleTokenExchangeRefused", "ConsoleTokenExchangeRefused", 403);
}
var OverlayCache = class {
  entries = /* @__PURE__ */ new Map();
  read;
  clock;
  constructor(read, clock = Date.now) {
    this.read = read;
    this.clock = clock;
  }
  async get(store) {
    const cached = this.entries.get(store);
    if (cached && this.clock() < cached.until) return cached.value ?? "";
    const value = (await this.read(store))?.slice(0, 8e3) ?? null;
    this.entries.set(store, { until: this.clock() + 3e5, value });
    return value ?? "";
  }
};
var StreamingRedactor = class {
  pending = "";
  denylist;
  constructor(denylist) {
    if (denylist.some((entry) => entry.length > 128)) throw new RangeError("denylist entry exceeds 128 chars");
    this.denylist = [...new Set(denylist.filter(Boolean))].sort((a, b) => b.length - a.length);
  }
  push(chunk) {
    this.pending += chunk;
    return this.release(Math.max(0, this.pending.length - 128));
  }
  finish() {
    return this.release(this.pending.length);
  }
  release(until) {
    let output = "";
    let index = 0;
    while (index < until) {
      const denied = this.denylist.find((entry) => this.pending.startsWith(entry, index));
      if (denied) {
        output += "[redacted]";
        index += denied.length;
      } else {
        output += this.pending[index];
        index++;
      }
    }
    this.pending = this.pending.slice(index);
    return output;
  }
};
function createTurn(options) {
  options.packs?.forEach(validatePack);
  const overlay = new OverlayCache(options.overlay ?? (async () => null), options.clock);
  return {
    async ask(request) {
      const vantage = request.vantage === void 0 ? void 0 : parseVantage(request.vantage);
      const admitted = options.tools.filter((tool) => request.packs.includes(tool.pack));
      const results = [];
      const rowsByTable = /* @__PURE__ */ new Map();
      const deadline = Date.now() + Math.min(options.timeoutMs ?? 1e4, 1e4);
      for (let round = 0; round < 2; round++) {
        const memoryTable = (name) => options.tables?.some((table) => table.kind === "memory" && table.name === name);
        const calls = await options.planner({
          question: request.question,
          vantage,
          tools: admitted.filter((tool) => tool.kind === "read" && !memoryTable(tool.table)),
          tables: (options.tables ?? []).filter((table) => table.kind === "data"),
          previous: results
        });
        for (const call of calls) {
          const tool = options.tools.find((candidate) => candidate.name === call.tool);
          if (!tool || !request.packs.includes(tool.pack)) throw new ConsoleError("ConsoleToolNotAdmitted", call.tool);
          if (tool.kind !== "read") throw new ConsoleError("ConsoleMutatingToolRequested", call.tool);
          if (memoryTable(tool.table)) throw new ConsoleError("ConsolePlannerReachedMemory", call.tool);
          if (tool.access === "direct-file") throw new ConsoleError("ConsoleFileAccessDirect", call.tool);
          if (tool.leg === "web" && call.publicationBound !== void 0) parseWebBound(call.publicationBound);
          const table = tool.leg === "web" ? void 0 : tool.table;
          if (tool.leg !== "web" && (!table || !options.tables?.some((entry) => entry.kind === "data" && entry.name === table))) {
            throw new RangeError(`tool ${call.tool} has no registered data table`);
          }
          const consumed = table === void 0 ? 0 : rowsByTable.get(table) ?? 0;
          const remaining = 5e3 - consumed;
          if (remaining <= 0) throw new RangeError("tool exceeded 5000 rows per data table");
          const controller = new AbortController();
          const milliseconds = Math.max(0, deadline - Date.now());
          let timeout;
          try {
            const result = await Promise.race([
              options.transport.call({ ...call, vantage, maxRows: remaining, signal: controller.signal }),
              new Promise((_, reject) => {
                timeout = setTimeout(() => {
                  controller.abort();
                  reject(new Error("code path exceeded 10 s"));
                }, milliseconds);
              })
            ]);
            if (result.rows.length > remaining) throw new RangeError("tool returned more than 5000 rows per data table");
            if (table !== void 0) rowsByTable.set(table, consumed + result.rows.length);
            results.push(result);
          } finally {
            if (timeout) clearTimeout(timeout);
          }
        }
        if (results.some((result) => result.rows.length > 0)) break;
      }
      if (!results.some((result) => result.rows.length > 0)) throw new ConsoleError("ConsoleUngroundedAnswer", "The store holds no matching information.");
      const sources = [...new Map(results.flatMap((result) => result.sources).map((source2) => [source2.id, source2])).values()].slice(0, 8);
      if (sources.length === 0) throw new ConsoleError("ConsoleUngroundedAnswer", "The store holds no sourced information.");
      const redactor = new StreamingRedactor(options.denylist ?? []);
      let answer = "";
      for await (const chunk of options.synthesize({ question: request.question, vantage, results, overlay: await overlay.get(request.store ?? "default"), sources })) {
        answer += redactor.push(chunk);
      }
      answer += redactor.finish();
      const citations = new Map(sources.map((source2, index) => [source2.id, `source-${index + 1}`]));
      answer = answer.replace(/\[([^\]\n]+)\]/g, (match, id) => id === "redacted" ? match : citations.has(id) ? `[${citations.get(id)}]` : "");
      const safeSources = sources.map((source2, index) => {
        const labelRedactor = new StreamingRedactor(options.denylist ?? []);
        const label = labelRedactor.push(source2.label) + labelRedactor.finish();
        const safeLabel = label.replace(/[\r\n]/g, " ");
        const url = source2.url && /^https?:\/\//.test(source2.url) && !(options.denylist ?? []).some((entry) => entry && source2.url?.includes(entry)) ? source2.url : void 0;
        return { id: `source-${index + 1}`, label: safeLabel, url };
      });
      const sourceLines = safeSources.map((source2) => `- ${source2.label}${source2.url ? ` (${source2.url})` : ""}`);
      return { text: `${answer.trim()}

Sources
${sourceLines.join("\n")}`, sources: safeSources };
    }
  };
}

// src/live.ts
function record(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function strings(value) {
  return Array.isArray(value) ? value.filter((item) => typeof item === "string") : [];
}
function terms(value) {
  return new Set((value.toLowerCase().match(/[a-z0-9]+/g) ?? []).map((word) => word.endsWith("ies") && word.length > 4 ? `${word.slice(0, -3)}y` : word.endsWith("s") && word.length > 3 ? word.slice(0, -1) : word));
}
function selectTable(question, listing) {
  const asked = terms(question);
  const entries = Array.isArray(listing) ? listing : [];
  const ranked = entries.flatMap((item) => {
    if (!record(item) || typeof item.table !== "string" || item.kind !== "data") return [];
    const name = terms(item.table.split("/").at(-1) ?? "");
    const description = terms(typeof item.description === "string" ? item.description : "");
    const score = [...asked].reduce((sum, word) => sum + (name.has(word) ? 2 : description.has(word) ? 1 : 0), 0);
    return score > 0 ? [{ table: item.table, score }] : [];
  }).sort((a, b) => b.score - a.score);
  return ranked.length > 0 && (ranked.length === 1 || ranked[0].score > ranked[1].score) ? ranked[0].table : null;
}
function source(columns, row, table) {
  const at = (name) => row[columns.indexOf(name)];
  const idColumn = columns.find((name) => name.endsWith("_id") || name === "id");
  const urlColumn = columns.find((name) => name === "source_url" || name === "url");
  const id = idColumn && at(idColumn);
  const url = urlColumn && at(urlColumn);
  if (typeof id !== "string" || !id) return null;
  const label = columns.includes("title") ? at("title") : columns.includes("summary") ? at("summary") : id;
  return {
    id,
    label: typeof label === "string" && label ? label : `${table}: ${id}`,
    ...typeof url === "string" && /^https?:\/\//.test(url) ? { url } : {}
  };
}
async function openReader({ stores, env, fetcher = fetch }, input) {
  const store = stores.find((entry) => entry.id === input.store);
  if (!store) throw new ConsoleError("ConsoleRequestMalformed");
  const shared = env[store.credentialName];
  const credential = store.auth === "exchange" ? await resolveReaderCredential({
    shared,
    mint: async () => {
      if (!input.operator.assertion || !store.exchangeRoute) throw new ConsoleError("ConsoleTokenExchangeUnavailable", "reader assertion or exchange route absent", 503);
      const response = await fetcher(new URL(store.exchangeRoute, store.endpoint), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ jwt: input.operator.assertion })
      });
      if (!response.ok) {
        const refusal2 = await response.json();
        const error = record(refusal2) ? refusal2.error : void 0;
        if ((response.status === 401 || response.status === 403) && record(error) && typeof error.identifier === "string") {
          throw new ConsoleError("ConsoleTokenExchangeRefused", error.identifier, 403);
        }
        throw new ConsoleError("ConsoleTokenExchangeUnavailable", `exchange answered ${response.status}`, 503);
      }
      const value = await response.json();
      if (!record(value) || typeof value.token !== "string" || !value.token.trim()) throw new ConsoleError("ConsoleTokenExchangeUnavailable", "exchange returned no credential", 503);
      return value.token;
    }
  }) : shared;
  if (!credential) throw new ConsoleError("ConsoleTokenExchangeRefused");
  let id = 0;
  const call = async (name, args, signal) => {
    const response = await fetcher(new URL("/mcp", store.endpoint), {
      method: "POST",
      signal,
      headers: { "Authorization": `Bearer ${credential}`, "Content-Type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: ++id, method: "tools/call", params: { name, arguments: args } })
    });
    if (!response.ok) throw new ConsoleError("ConsoleStoreReadRefused", `store answered ${response.status}`, response.status);
    const message = await response.json();
    if (!record(message) || !record(message.result) || message.result.isError === true || !record(message.result.structuredContent)) {
      throw new ConsoleError("ConsoleStoreReadRefused");
    }
    return message.result.structuredContent;
  };
  return { store, call };
}
function createLiveTurn(options) {
  const { env, fetcher = fetch } = options;
  const modelEndpoint = env.CONTEXTFUL_MODEL_ENDPOINT;
  const modelId = env.CONTEXTFUL_MODEL_ID;
  if (!modelEndpoint || !modelId) throw new Error("ConsoleModelUnconfigured");
  return async (input) => {
    const { store, call } = await openReader(options, input);
    const description = await call("context.describe", {});
    const selected = selectTable(input.question, description.tables);
    if (!selected) throw new ConsoleError("ConsoleUngroundedAnswer", "No data table matches this question.");
    const tables = [selected];
    let remainingSources = 8;
    const turn = createTurn({
      tools: tables.map((table) => ({ name: `read:${table}`, pack: "data", kind: "read", table })),
      tables: tables.map((name) => ({ name, kind: "data" })),
      planner: async ({ previous }) => previous.length ? [] : tables.map((table) => ({ tool: `read:${table}`, arguments: {} })),
      transport: { call: async ({ tool, maxRows, signal }) => {
        const table = tool.slice("read:".length);
        const result = await call("context.query", { sql: `SELECT * FROM "${table.replaceAll('"', '""')}"`, limit: Math.min(maxRows, remainingSources) }, signal);
        const columns = strings(result.columns);
        const rawRows = Array.isArray(result.rows) ? result.rows.filter(Array.isArray) : [];
        const sourced = rawRows.flatMap((row) => {
          const citation = source(columns, row, table);
          return citation ? [{ row, citation }] : [];
        }).slice(0, remainingSources);
        remainingSources -= sourced.length;
        return {
          rows: sourced.map(({ row }) => Object.fromEntries(columns.map((column, index) => [column, row[index]]))),
          sources: sourced.map(({ citation }) => citation)
        };
      } },
      synthesize: async function* ({ question, results, sources }) {
        const response = await fetcher(new URL("chat/completions", `${modelEndpoint.replace(/\/$/, "")}/`), {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ model: modelId, messages: [
            { role: "system", content: "Answer only from the supplied governed rows. Cite source IDs in square brackets. Decline unsupported claims." },
            { role: "user", content: JSON.stringify({ question, rows: results.flatMap((result) => result.rows), sources }) }
          ] })
        });
        if (!response.ok) throw new ConsoleError("ConsoleModelUnavailable", `model answered ${response.status}`, 503);
        const body2 = await response.json();
        const choices = record(body2) && Array.isArray(body2.choices) ? body2.choices : [];
        const message = choices.length && record(choices[0]) ? choices[0].message : void 0;
        if (!record(message) || typeof message.content !== "string") throw new ConsoleError("ConsoleModelUnavailable", "model answer absent", 503);
        yield message.content;
      }
    });
    const answer = await turn.ask({ question: input.question, packs: ["data"], store: store.id });
    return { answer: answer.text, sources: answer.sources, widgets: [] };
  };
}

// src/browse.ts
var MAX_TABLES = 64;
var MAX_FILES = 128;
var MAX_PREVIEW_ROWS = 100;
var BrowseError = class extends Error {
  code;
  constructor(code, message = code) {
    super(message);
    this.name = "BrowseError";
    this.code = code;
  }
};
var abbreviations = /* @__PURE__ */ new Set(["API", "CSV", "PDF", "SEC", "SQL", "URL"]);
function humanizeLabel(identifier) {
  const tail = identifier.split("/").at(-1) ?? identifier;
  return tail.replace(/\.[^.]+$/, "").replace(/([a-z])([A-Z])/g, "$1 $2").split(/[-_\s]+/).filter(Boolean).map((part) => {
    const upper = part.toUpperCase();
    return abbreviations.has(upper) || /^[Qq]\d+$/.test(part) ? upper : part[0].toUpperCase() + part.slice(1).toLowerCase();
  }).join(" ");
}
function object(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new BrowseError("ConsoleBrowseResponseMalformed");
  return value;
}
function bounds(request) {
  return request.asOf ? { as_of: request.asOf } : {};
}
function createBrowse(transport) {
  async function read(tool, arguments_) {
    const result = await transport.call(tool, arguments_);
    if (result.isError) {
      const error = object(object(result.structuredContent).error);
      throw new BrowseError(String(error.identifier ?? "ConsoleBrowseReadRefused"));
    }
    return object(result.structuredContent);
  }
  async function listed(request) {
    const listing = await read("context.describe", bounds(request));
    if (!Array.isArray(listing.tables)) throw new BrowseError("ConsoleBrowseResponseMalformed");
    const tables = listing.tables.map((item) => object(item)).filter((item) => item.zone_admitted === true && item.kind !== "memory" && typeof item.table === "string").slice(0, MAX_TABLES).map((item) => ({ table: item.table, description: typeof item.description === "string" ? item.description : void 0 }));
    const names2 = new Set(tables.map((table) => table.table));
    const fileListing = await read("context.files", bounds(request));
    if (!Array.isArray(fileListing.rows)) throw new BrowseError("ConsoleBrowseResponseMalformed");
    const files = fileListing.rows.filter((row) => Array.isArray(row) && typeof row[0] === "string" && typeof row[1] === "string" && names2.has(row[0])).slice(0, MAX_FILES).map(([table, path]) => ({ table, path, label: humanizeLabel(path) }));
    return { tables, files };
  }
  return {
    async discover(request) {
      const { tables, files } = await listed(request);
      const chips = tables.map((table) => ({ table: table.table, label: humanizeLabel(table.table), description: table.description }));
      const descriptions = await Promise.all(tables.map((table) => read("context.describe", { table: table.table, ...bounds(request) })));
      const insights = tables.flatMap((table, index) => {
        const count = descriptions[index].row_count;
        return typeof count === "number" && Number.isFinite(count) && count >= 0 ? [{ table: table.table, label: humanizeLabel(table.table), rows: count }] : [];
      });
      return { chips, insights, files };
    },
    async preview(request) {
      const { files } = await listed(request);
      if (!files.some((file) => file.path === request.path)) throw new BrowseError("ConsoleGalleryPathUnlisted", request.path);
      const response = await read("context.file", { path: request.path, limit: MAX_PREVIEW_ROWS, ...bounds(request) });
      if (!Array.isArray(response.columns) || !Array.isArray(response.rows)) throw new BrowseError("ConsoleBrowseResponseMalformed");
      return { columns: response.columns.slice(0, 64), rows: response.rows.slice(0, MAX_PREVIEW_ROWS).map((row) => Array.isArray(row) ? row.slice(0, 64) : []) };
    }
  };
}

// src/live_browse.ts
function createLiveBrowse(options) {
  async function session(input) {
    const { call } = await openReader(options, input);
    return createBrowse({ call: async (name, args) => ({ structuredContent: await call(name, args) }) });
  }
  return {
    async discover(input) {
      return (await session(input)).discover({ asOf: input.asOf });
    },
    async preview(input) {
      return (await session(input)).preview({ asOf: input.asOf, path: input.path });
    }
  };
}

// src/server.ts
import { createServer } from "node:http";

// src/index.ts
import { createHash, createHmac, createPublicKey, randomBytes, timingSafeEqual, verify } from "node:crypto";
function encoded(value) {
  return Buffer.from(JSON.stringify(value)).toString("base64url");
}
function decode(value) {
  return JSON.parse(Buffer.from(value, "base64url").toString("utf8"));
}
function object2(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
function secureEqual(left, right) {
  return left.length === right.length && timingSafeEqual(left, right);
}
function issueCognitoSession(session, secret) {
  if (!secret || !session.subject || !Number.isSafeInteger(session.expiresAt) || session.expiresAt <= Math.floor(Date.now() / 1e3)) {
    throw new Error("CognitoSessionInvalid");
  }
  const body2 = encoded(session);
  const signature = createHmac("sha256", secret).update(body2).digest("base64url");
  return `${body2}.${signature}`;
}
function verifyCognitoSession(cookie, identity2) {
  const token = cookie.split("; ").find((part) => part.startsWith("console_session="))?.slice("console_session=".length);
  if (!token) return null;
  const parts = token.split(".");
  if (parts.length !== 2) return null;
  const expected = createHmac("sha256", identity2.sessionSecret).update(parts[0]).digest();
  let actual;
  try {
    actual = Buffer.from(parts[1], "base64url");
  } catch {
    return null;
  }
  if (!secureEqual(expected, actual)) return null;
  let session;
  try {
    session = decode(parts[0]);
  } catch {
    return null;
  }
  if (!object2(session) || typeof session.subject !== "string" || !session.subject || !Number.isSafeInteger(session.expiresAt) || session.expiresAt <= Math.floor(Date.now() / 1e3) || !Array.isArray(session.groups) || !session.groups.every((group) => typeof group === "string") || session.assertion !== void 0 && typeof session.assertion !== "string") return null;
  const grants = /* @__PURE__ */ new Set();
  if (session.groups.includes(identity2.queryGroup)) grants.add("query");
  if (session.groups.includes(identity2.adminGroup)) grants.add("admin");
  return { subject: session.subject, grants, assertion: typeof session.assertion === "string" ? session.assertion : void 0 };
}
function verifyAccess(assertion, identity2) {
  const parts = assertion.split(".");
  if (parts.length !== 3) return null;
  let header;
  let claims;
  try {
    header = decode(parts[0]);
    claims = decode(parts[1]);
  } catch {
    return null;
  }
  if (!object2(header) || header.alg !== "RS256" || !object2(claims)) return null;
  if (claims.iss !== identity2.issuer || typeof claims.sub !== "string" || !claims.sub || !Number.isSafeInteger(claims.exp) || claims.exp <= Math.floor(Date.now() / 1e3)) return null;
  if (typeof claims.nbf === "number" && claims.nbf > Math.floor(Date.now() / 1e3)) return null;
  const audience = typeof claims.aud === "string" ? [claims.aud] : Array.isArray(claims.aud) ? claims.aud : [];
  const grants = /* @__PURE__ */ new Set();
  if (audience.includes(identity2.queryAudience)) grants.add("query");
  if (audience.includes(identity2.adminAudience)) grants.add("admin");
  if (grants.size === 0) return null;
  let signature;
  try {
    signature = Buffer.from(parts[2], "base64url");
  } catch {
    return null;
  }
  const key = identity2.keys ? typeof header.kid === "string" ? identity2.keys.get(header.kid) : void 0 : typeof identity2.publicKey === "string" ? createPublicKey(identity2.publicKey) : identity2.publicKey;
  if (!key) return null;
  if (!verify("RSA-SHA256", Buffer.from(`${parts[0]}.${parts[1]}`), key, signature)) return null;
  return { subject: claims.sub, grants, assertion };
}
function operatorFor(request, identity2) {
  if (identity2.kind === "access") {
    const assertion = request.headers.get("cf-access-jwt-assertion");
    return assertion ? verifyAccess(assertion, identity2) : null;
  }
  return verifyCognitoSession(request.headers.get("cookie") ?? "", identity2);
}
function cognitoLogin(identity2) {
  if (!identity2.authorizeUrl || !identity2.clientId || !identity2.redirectUri) return refusal("ConsoleLoginUnconfigured", 503);
  const state = randomBytes(24).toString("base64url");
  const verifier = randomBytes(32).toString("base64url");
  const issued = Math.floor(Date.now() / 1e3).toString();
  const loginData = `${state}.${verifier}.${issued}`;
  const signature = createHmac("sha256", identity2.sessionSecret).update(loginData).digest("base64url");
  const location = new URL(identity2.authorizeUrl);
  location.searchParams.set("response_type", "code");
  location.searchParams.set("client_id", identity2.clientId);
  location.searchParams.set("redirect_uri", identity2.redirectUri);
  location.searchParams.set("scope", "openid email profile");
  location.searchParams.set("state", state);
  location.searchParams.set("code_challenge", createHash("sha256").update(verifier).digest("base64url"));
  location.searchParams.set("code_challenge_method", "S256");
  return new Response(null, { status: 302, headers: {
    Location: location.toString(),
    "Set-Cookie": `console_login_state=${loginData}.${signature}; HttpOnly; Secure; SameSite=Lax; Path=/auth/callback; Max-Age=300`,
    "Cache-Control": "no-store"
  } });
}
async function cognitoCallback(request, identity2) {
  if (!identity2.tokenUrl || !identity2.clientId || !identity2.redirectUri || !identity2.issuer || !identity2.publicKey && !identity2.keys) return refusal("ConsoleLoginUnconfigured", 503);
  const url = new URL(request.url);
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  const loginCookie = request.headers.get("cookie")?.split(/;\s*/).find((part) => part.startsWith("console_login_state="))?.slice("console_login_state=".length);
  const loginParts = loginCookie?.split(".");
  if (!code || !state || !loginParts || loginParts.length !== 4) return refusal("ConsolePageForbidden");
  const [cookieState, verifier, issued, signature] = loginParts;
  const expected = createHmac("sha256", identity2.sessionSecret).update(`${cookieState}.${verifier}.${issued}`).digest("base64url");
  const age = Math.floor(Date.now() / 1e3) - Number(issued);
  if (!secureEqual(Buffer.from(state), Buffer.from(cookieState)) || !secureEqual(Buffer.from(signature), Buffer.from(expected)) || !Number.isInteger(age) || age < 0 || age > 300 || !/^[A-Za-z0-9_-]{43}$/.test(verifier)) return refusal("ConsolePageForbidden");
  const exchange = await (identity2.fetcher ?? fetch)(identity2.tokenUrl, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({ grant_type: "authorization_code", client_id: identity2.clientId, redirect_uri: identity2.redirectUri, code, code_verifier: verifier })
  });
  if (!exchange.ok) return refusal("ConsolePageForbidden");
  const tokens = await exchange.json();
  if (!object2(tokens) || typeof tokens.id_token !== "string") return refusal("ConsolePageForbidden");
  const parts = tokens.id_token.split(".");
  if (parts.length !== 3) return refusal("ConsolePageForbidden");
  let header;
  let claims;
  try {
    header = decode(parts[0]);
    claims = decode(parts[1]);
  } catch {
    return refusal("ConsolePageForbidden");
  }
  if (!object2(header) || header.alg !== "RS256" || !object2(claims) || claims.iss !== identity2.issuer || claims.aud !== identity2.clientId || typeof claims.sub !== "string" || !claims.sub || !Number.isSafeInteger(claims.exp) || claims.exp <= Math.floor(Date.now() / 1e3) || claims.token_use !== "id") return refusal("ConsolePageForbidden");
  const key = identity2.keys ? typeof header.kid === "string" ? identity2.keys.get(header.kid) : void 0 : typeof identity2.publicKey === "string" ? createPublicKey(identity2.publicKey) : identity2.publicKey;
  if (!key) return refusal("ConsolePageForbidden");
  if (!verify("RSA-SHA256", Buffer.from(`${parts[0]}.${parts[1]}`), key, Buffer.from(parts[2], "base64url"))) return refusal("ConsolePageForbidden");
  const groups = Array.isArray(claims["cognito:groups"]) ? claims["cognito:groups"].filter((group) => typeof group === "string") : [];
  const lifetime = Math.min(claims.exp, Math.floor(Date.now() / 1e3) + 3600);
  const session = issueCognitoSession({ subject: claims.sub, groups, expiresAt: lifetime, assertion: tokens.id_token }, identity2.sessionSecret);
  const destination = groups.includes(identity2.adminGroup) ? "/admin" : "/query";
  return new Response(null, { status: 302, headers: {
    Location: destination,
    "Set-Cookie": `console_session=${session}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=${lifetime - Math.floor(Date.now() / 1e3)}`,
    "Cache-Control": "no-store"
  } });
}
function refusal(identifier, status = 403) {
  return Response.json({ error: { identifier } }, { status, headers: { "Cache-Control": "no-store" } });
}
function json(value) {
  return Response.json(value, { headers: { "Cache-Control": "no-store" } });
}
async function body(request) {
  const limit = 1048576;
  if (Number(request.headers.get("content-length") ?? 0) > limit) throw new Error("ConsoleBodyTooLarge");
  const chunks = [];
  let size = 0;
  const reader = request.body?.getReader();
  if (!reader) return JSON.parse("");
  try {
    for (; ; ) {
      const { done, value } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > limit) {
        void reader.cancel().catch(() => {
        });
        throw new Error("ConsoleBodyTooLarge");
      }
      chunks.push(Buffer.from(value));
    }
  } finally {
    reader.releaseLock();
  }
  return JSON.parse(Buffer.concat(chunks, size).toString("utf8"));
}
function bodyFailure(error) {
  return error instanceof Error && error.message === "ConsoleBodyTooLarge" ? refusal("ConsoleBodyTooLarge", 413) : refusal("ConsoleRequestMalformed", 400);
}
function page(name) {
  return new Response(name === "query" ? queryPage : adminPage, {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-store",
      "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'",
      "X-Content-Type-Options": "nosniff"
    }
  });
}
function createConsole(adapters) {
  return {
    async fetch(request) {
      const url = new URL(request.url);
      const path = url.pathname;
      if (adapters.identity.kind === "cognito") {
        if (path === "/auth/login" && request.method === "GET") return cognitoLogin(adapters.identity);
        if (path === "/auth/callback" && request.method === "GET") return cognitoCallback(request, adapters.identity);
      }
      const grant = path === "/query" || path.startsWith("/query/api/") ? "query" : path === "/admin" || path.startsWith("/admin/api/") ? "admin" : null;
      if (!grant) return refusal("ConsoleRouteNotFound", 404);
      let operator;
      try {
        operator = operatorFor(request, adapters.identity);
      } catch {
        operator = null;
      }
      if (!operator) return refusal("ConsolePageForbidden", 401);
      if (!operator.grants.has(grant)) return refusal("ConsolePageForbidden");
      if (request.method === "POST" && request.headers.get("origin") !== url.origin) {
        return refusal("ConsolePageForbidden");
      }
      if (request.method === "GET" && path === `/${grant}`) return page(grant);
      if (grant === "query") {
        if (request.method === "GET" && path === "/query/api/stores") return json(await adapters.read.list(operator));
        if (request.method === "POST" && (path === "/query/api/browse" || path === "/query/api/preview")) {
          let input;
          try {
            input = await body(request);
          } catch (error) {
            return bodyFailure(error);
          }
          if (!object2(input) || typeof input.store !== "string" || !adapters.stores.some((store) => store.id === input.store) || input.asOf !== void 0 && typeof input.asOf !== "string" || path.endsWith("/preview") && (typeof input.path !== "string" || !input.path || input.path.length > 4096)) {
            return refusal("ConsoleRequestMalformed", 400);
          }
          let asOf;
          try {
            asOf = input.asOf === void 0 ? void 0 : parseVantage(input.asOf);
          } catch {
            return refusal("ConsoleVantageUnparseable", 400);
          }
          if (!adapters.browse) return refusal("ConsoleAdapterUnavailable", 503);
          try {
            return json(path.endsWith("/preview") ? await adapters.browse.preview({ operator, store: input.store, asOf, path: input.path }) : await adapters.browse.discover({ operator, store: input.store, asOf }));
          } catch (error) {
            if (error instanceof BrowseError) return refusal(error.code, error.code === "ConsoleGalleryPathUnlisted" ? 403 : 400);
            if (error instanceof ConsoleError) return refusal(error.code, error.status);
            throw error;
          }
        }
        if (request.method === "POST" && path === "/query/api/ask") {
          let input;
          try {
            input = await body(request);
          } catch (error) {
            return bodyFailure(error);
          }
          if (!object2(input) || typeof input.store !== "string" || typeof input.question !== "string" || !input.question.trim() || !adapters.stores.some((store) => store.id === input.store)) return refusal("ConsoleRequestMalformed", 400);
          try {
            return json(await adapters.turn({ operator, store: input.store, question: input.question }));
          } catch (error) {
            if (error instanceof ConsoleError) return refusal(error.code, error.status);
            throw error;
          }
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
          let document;
          try {
            document = await body(request);
          } catch (error) {
            return bodyFailure(error);
          }
          return json(path.endsWith("/edit") ? await adapters.control.edit(document, adapters.adminCapability, operator) : await adapters.control.apply(document, adapters.adminCapability, operator));
        }
      }
      return refusal("ConsoleRouteNotFound", 404);
    }
  };
}
var sharedStyle = `<style>
:root{font-family:ui-sans-serif,system-ui,sans-serif;color:#182b39;background:#e7edf0}*{box-sizing:border-box}body{margin:0;min-height:100vh}.shell{display:grid;grid-template-columns:210px minmax(0,1fr);min-height:100vh}nav{background:#173b50;color:#eaf4f5;padding:26px 20px}nav strong{font-size:1.3rem;letter-spacing:-.04em}nav a{display:block;color:#eaf4f5;text-decoration:none;margin-top:24px;padding:9px 11px;border-radius:7px}nav a[aria-current]{background:#31657b}main{padding:30px min(5vw,64px);max-width:1200px;width:100%}h1{font-size:clamp(2rem,4vw,3rem);letter-spacing:-.055em;margin:5px 0 12px}h2{font-size:1.1rem}p{line-height:1.5}section{background:#fff;border:1px solid #c8d5da;border-radius:12px;padding:20px;margin:18px 0}button,select,textarea{font:inherit}button{background:#125a72;color:white;border:0;border-radius:7px;padding:10px 16px;cursor:pointer}button:focus-visible,a:focus-visible,select:focus-visible,textarea:focus-visible{outline:3px solid #efae4c;outline-offset:2px}textarea{width:100%;min-height:100px;padding:12px;border:1px solid #91a8b2;border-radius:7px}select{padding:8px;border:1px solid #91a8b2;border-radius:7px}pre{white-space:pre-wrap;overflow-wrap:anywhere}.muted{color:#536b78}.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(240px,1fr));gap:14px}.node{border:1px solid #afc5cf;border-left:5px solid #3c7c92;border-radius:7px;padding:13px;background:#f5f9fa}#transcript article{border-left:3px solid #3c7c92;padding:8px 16px;margin:12px 0}#widgets table{border-collapse:collapse;width:100%}#widgets td,#widgets th{border-bottom:1px solid #c8d5da;padding:8px;text-align:left}@media(max-width:650px){.shell{display:block}nav{display:flex;gap:12px;align-items:center;padding:12px 20px}nav a{margin:0}main{padding:20px}}@media(prefers-reduced-motion:reduce){*{scroll-behavior:auto!important}}
</style>`;
var queryPage = `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Query \xB7 Contextful</title>${sharedStyle}<div class="shell"><nav aria-label="Console"><strong>Contextful</strong><a href="/query" aria-current="page">Query</a><a href="/admin">Admin</a></nav><main><h1>Ask the store</h1><p class="muted">Answers cite the rows your access permits.</p><section><label for="store">Store</label> <select id="store" required></select> <label for="as-of">As of</label> <input id="as-of" type="date"><p id="browse-status" role="status"></p><h2>Explore</h2><div id="chips" class="grid"></div><div id="insights" class="grid"></div><h2>Files</h2><div id="file-gallery" class="grid"></div><div id="file-preview"></div></section><section><form id="composer"><p><label for="question">Question</label></p><textarea id="question" required></textarea><p><button type="submit">Ask question</button></p></form></section><section><h2>Conversation</h2><div id="transcript" role="log" aria-live="polite"><p class="muted">Your answer appears here.</p></div></section><section><h2>Results</h2><div id="widgets"></div></section></main></div><script>
const form=document.getElementById('composer'),store=document.getElementById('store'),asOf=document.getElementById('as-of'),questionField=document.getElementById('question'),transcript=document.getElementById('transcript'),widgets=document.getElementById('widgets'),chips=document.getElementById('chips'),insights=document.getElementById('insights'),gallery=document.getElementById('file-gallery'),filePreview=document.getElementById('file-preview'),browseStatus=document.getElementById('browse-status');
async function post(path,payload){const response=await fetch(path,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(payload)});const value=await response.json();if(!response.ok)throw new Error(value.error?.identifier??'Request failed');return value}
function current(){return {store:store.value,...(asOf.value?{asOf:asOf.value}:{})}}
function draw(widget){const props=widget.props??{};if(widget.component==='table.v1'&&Array.isArray(props.columns)&&Array.isArray(props.rows)){const table=document.createElement('table'),head=document.createElement('thead'),header=document.createElement('tr'),body=document.createElement('tbody');for(const column of props.columns){const cell=document.createElement('th');cell.textContent=String(column);header.append(cell)}head.append(header);for(const row of props.rows){const line=document.createElement('tr');for(const value of row){const cell=document.createElement('td');cell.textContent=String(value??'');line.append(cell)}body.append(line)}table.append(head,body);return table}if(widget.component==='metric.v1'){const metric=document.createElement('p');metric.style.fontSize='2rem';metric.textContent=String(props.value??'');return metric}const fallback=document.createElement('pre');fallback.textContent=JSON.stringify(props,null,2);return fallback}
async function refreshBrowse(){chips.replaceChildren();insights.replaceChildren();gallery.replaceChildren();filePreview.replaceChildren();browseStatus.textContent='';if(!store.value)return;try{const data=await post('/query/api/browse',current());for(const chip of data.chips??[]){const button=document.createElement('button');button.type='button';button.textContent=chip.label;button.title=chip.description??'';button.addEventListener('click',()=>{questionField.value='Tell me about '+chip.label;questionField.focus()});chips.append(button)}for(const insight of data.insights??[]){const card=document.createElement('p');card.textContent=insight.label+': '+insight.rows+' rows';insights.append(card)}for(const file of data.files??[]){const button=document.createElement('button');button.type='button';button.textContent=file.label;button.addEventListener('click',async()=>{filePreview.replaceChildren();try{const preview=await post('/query/api/preview',{...current(),path:file.path});filePreview.append(draw({component:'table.v1',props:preview}))}catch(error){browseStatus.textContent=String(error)}});gallery.append(button)}}catch(error){browseStatus.textContent=String(error)}}
fetch('/query/api/stores').then(r=>r.json()).then(rows=>{for(const row of rows){const option=document.createElement('option');option.value=row.id;option.textContent=row.label;store.append(option)}void refreshBrowse()});
store.addEventListener('change',()=>{void refreshBrowse()});asOf.addEventListener('change',()=>{void refreshBrowse()});
form.addEventListener('submit',async event=>{event.preventDefault();const question=questionField.value.trim();if(!question)return;const response=await fetch('/query/api/ask',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({store:store.value,question})});const answer=await response.json();transcript.replaceChildren();const item=document.createElement('article');item.textContent=question+'\\n'+(answer.answer??answer.error?.identifier??'No answer');transcript.append(item);widgets.replaceChildren();for(const widget of answer.widgets??[])widgets.append(draw(widget));if(answer.sources?.length){const sources=document.createElement('p');sources.textContent='Sources: '+answer.sources.map(x=>x.title??x.url).join(', ');transcript.append(sources)}});
</script></html>`;
var adminPage = `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Admin \xB7 Contextful</title>${sharedStyle}<div class="shell"><nav aria-label="Console"><strong>Contextful</strong><a href="/query">Query</a><a href="/admin" aria-current="page">Admin</a></nav><main><h1>Store operations</h1><p class="muted">Pipelines, schedules, steps and run outcomes come from the store.</p><section><h2>Workflow canvas</h2><div id="canvas" class="grid"></div></section><section><h2>Operational record</h2><div id="record"></div></section><section><h2>Control document</h2><label for="document">Document</label><textarea id="document"></textarea><p><button id="edit">Edit</button> <button id="apply">Apply</button></p><p id="result" role="status"></p></section></main></div><script>
const canvas=document.getElementById('canvas'),record=document.getElementById('record');fetch('/admin/api/workflows').then(r=>r.json()).then(data=>{for(const pipeline of data.pipelines??[]){const node=document.createElement('article');node.className='node';node.textContent=[pipeline.id,pipeline.schedule,...(pipeline.steps??[]),...(pipeline.runs??[]).map(run=>run.status)].filter(Boolean).join(' \xB7 ');canvas.append(node)}if(!canvas.children.length)canvas.textContent='No workflows are published.'});fetch('/admin/api/record').then(r=>r.json()).then(data=>{record.textContent=JSON.stringify(data,null,2)});for(const action of ['edit','apply'])document.getElementById(action).addEventListener('click',async()=>{const text=document.getElementById('document').value;let documentValue;try{documentValue=JSON.parse(text)}catch{document.getElementById('result').textContent='Enter a valid JSON document.';return}const response=await fetch('/admin/api/'+action,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(documentValue)});document.getElementById('result').textContent=response.ok?action+' complete':(await response.json()).error?.identifier??'Request failed'});
</script></html>`;

// src/server.ts
var maxBodyBytes = 1048576;
async function receive(message, origin) {
  const method = message.method ?? "GET";
  const chunks = [];
  let size = 0;
  for await (const chunk of message) {
    size += chunk.length;
    if (size > maxBodyBytes) throw new Error("ConsoleBodyTooLarge");
    chunks.push(Buffer.from(chunk));
  }
  const body2 = method === "GET" || method === "HEAD" ? void 0 : Buffer.concat(chunks);
  const headers = new Headers();
  for (const [key, value] of Object.entries(message.headers)) {
    if (typeof value === "string") headers.set(key, value);
    else if (Array.isArray(value)) headers.set(key, value.join(", "));
  }
  return new Request(new URL(message.url ?? "/", origin), { method, headers, body: body2 });
}
async function send(reply, response) {
  reply.statusCode = response.status;
  response.headers.forEach((value, key) => reply.setHeader(key, value));
  reply.end(Buffer.from(await response.arrayBuffer()));
}
function serveConsole(adapters, origin) {
  const app = createConsole(adapters);
  return createServer(async (message, reply) => {
    try {
      await send(reply, await app.fetch(await receive(message, typeof origin === "string" ? origin : origin())));
    } catch (error) {
      if (error instanceof Error && error.message === "ConsoleBodyTooLarge") {
        await send(reply, Response.json({ error: { identifier: "ConsoleBodyTooLarge" } }, { status: 413 }));
        return;
      }
      const unavailable2 = error instanceof Error && error.message === "ConsoleAdapterUnavailable";
      await send(reply, Response.json({ error: { identifier: unavailable2 ? "ConsoleAdapterUnavailable" : "ConsoleServerFailure" } }, { status: unavailable2 ? 503 : 500 }));
    }
  });
}

// src/entry.ts
function required(name) {
  const value = process.env[name];
  if (!value) throw new Error(`${name} is required`);
  return value;
}
async function keysAt(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(`identity key route returned ${response.status}`);
  const document = await response.json();
  if (!document || typeof document !== "object" || !("keys" in document) || !Array.isArray(document.keys)) {
    throw new Error("identity key route returned no key set");
  }
  const keys = /* @__PURE__ */ new Map();
  for (const value of document.keys) {
    if (!value || typeof value !== "object" || value.kty !== "RSA" || value.alg !== "RS256" || typeof value.kid !== "string") continue;
    keys.set(value.kid, createPublicKey2({ key: value, format: "jwk" }));
  }
  if (keys.size === 0) throw new Error("identity key route returned no RS256 key");
  return keys;
}
async function identity() {
  if (process.env.CONTEXTFUL_IDENTITY === "cognito") {
    const keys = await keysAt(required("CONTEXTFUL_COGNITO_JWKS_URL"));
    return {
      kind: "cognito",
      sessionSecret: required("CONTEXTFUL_COGNITO_SESSION_SECRET"),
      queryGroup: required("CONTEXTFUL_COGNITO_QUERY_GROUP"),
      adminGroup: required("CONTEXTFUL_COGNITO_ADMIN_GROUP"),
      issuer: required("CONTEXTFUL_COGNITO_ISSUER"),
      clientId: required("CONTEXTFUL_COGNITO_CLIENT_ID"),
      authorizeUrl: required("CONTEXTFUL_COGNITO_AUTHORIZE_URL"),
      tokenUrl: required("CONTEXTFUL_COGNITO_TOKEN_URL"),
      redirectUri: required("CONTEXTFUL_COGNITO_REDIRECT_URI"),
      keys
    };
  }
  return {
    kind: "access",
    issuer: required("CONTEXTFUL_ACCESS_ISSUER"),
    queryAudience: required("CONTEXTFUL_QUERY_ACCESS_AUDIENCE"),
    adminAudience: required("CONTEXTFUL_ADMIN_ACCESS_AUDIENCE"),
    keys: await keysAt(required("CONTEXTFUL_ACCESS_JWKS_URL"))
  };
}
function unavailable() {
  throw new Error("ConsoleAdapterUnavailable");
}
function unavailableAdapters() {
  return {
    turn: async () => unavailable(),
    control: {
      workflows: async () => unavailable(),
      record: async () => unavailable(),
      edit: async () => unavailable(),
      apply: async () => unavailable()
    }
  };
}
async function main() {
  const addressFlag = process.argv.indexOf("--http");
  if (addressFlag < 0 || !process.argv[addressFlag + 1]) throw new Error("--http <host:port> is required");
  const address = process.argv[addressFlag + 1];
  const separator = address.lastIndexOf(":");
  const host = address.slice(0, separator);
  const port = Number(address.slice(separator + 1));
  if (!host || !Number.isInteger(port) || port < 0 || port > 65535) throw new Error("invalid --http address");
  const registry = registryFromEnv(process.env.CONTEXTFUL_STORES_JSON);
  const stores = registry.entries.map(({ id, label }) => ({ id, label }));
  const modulePath = process.env.CONTEXTFUL_CONSOLE_ADAPTER_MODULE;
  let adapters = {
    ...unavailableAdapters(),
    turn: modulePath ? unavailableAdapters().turn : createLiveTurn({ stores: registry.entries, env: process.env }),
    browse: modulePath ? void 0 : createLiveBrowse({ stores: registry.entries, env: process.env })
  };
  if (modulePath) {
    const absolute = isAbsolute(modulePath) ? modulePath : resolve(modulePath);
    const module = await import(pathToFileURL(absolute).href);
    if (!module.createAdapters) throw new Error("console adapter module exports no createAdapters");
    adapters = await module.createAdapters({ stores, env: process.env });
  }
  let origin = process.env.CONTEXTFUL_CONSOLE_ORIGIN ?? `http://${host}:${port}`;
  const app = serveConsole({
    identity: await identity(),
    stores,
    adminCapability: process.env.CONTEXTFUL_ADMIN_CAPABILITY,
    turn: adapters.turn,
    read: adapters.read ?? { list: async () => stores },
    browse: adapters.browse,
    control: adapters.control
  }, () => origin);
  await new Promise((resolveListen, rejectListen) => {
    app.once("error", rejectListen);
    app.listen(port, host, resolveListen);
  });
  const bound = app.address();
  if (!bound || typeof bound === "string") throw new Error("console listener has no address");
  if (!process.env.CONTEXTFUL_CONSOLE_ORIGIN) origin = `http://${host}:${bound.port}`;
  process.stderr.write(`listening on http://${host}:${bound.port}
`);
}
main().catch((error) => {
  process.stderr.write(`${error instanceof Error ? error.message : "console startup failed"}
`);
  process.exitCode = 1;
});
