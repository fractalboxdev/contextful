import type { StoreEntry } from "../../gateway/src/index.ts";
import type { Operator, TurnInput, TurnResult } from "./index.ts";
import { ConsoleError, createTurn, OverlayCache, resolveReaderCredential, StreamingRedactor, type Source, type ToolResult } from "./turn.ts";

export type LiveOptions = { stores: StoreEntry[]; env: NodeJS.ProcessEnv; fetcher?: typeof fetch; clock?: () => number };

export function consoleDenylist(env: NodeJS.ProcessEnv): string[] {
  if (!env.CONTEXTFUL_CONSOLE_DENYLIST) return [];
  const parsed: unknown = JSON.parse(env.CONTEXTFUL_CONSOLE_DENYLIST);
  if (!Array.isArray(parsed) || !parsed.every((entry) => typeof entry === "string")) throw new Error("ConsoleDenylistMalformed");
  return parsed;
}

export function redactText(value: string, denylist: string[]): string {
  const long = denylist.filter((entry) => entry.length > 128);
  const exact = long.reduce((text, entry) => text.replaceAll(entry, "[redacted]"), value);
  const redactor = new StreamingRedactor(denylist.filter((entry) => entry.length <= 128));
  return redactor.push(exact) + redactor.finish();
}

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function terms(value: string): Set<string> {
  return new Set((value.toLowerCase().match(/[a-z0-9]+/g) ?? []).map((word) =>
    word.endsWith("ies") && word.length > 4 ? `${word.slice(0, -3)}y` : word.endsWith("s") && word.length > 3 ? word.slice(0, -1) : word));
}

export function selectTable(question: string, listing: unknown): string | null {
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

const filler = new Set(["the", "and", "for", "with", "about", "tell", "me", "what", "which", "who", "when", "where", "why", "how",
  "is", "are", "was", "were", "do", "does", "did", "of", "on", "in", "to", "a", "an", "any", "show", "give", "latest", "news", "info",
  "information", "know", "can", "you", "please", "it", "its", "this", "that"]);

const textType = /^(varchar|text|string|char)/i;

export type TableRead = { table: string; where: string };

export function searchTables(question: string, listing: unknown): TableRead[] {
  const subjects = [...new Set(question.toLowerCase().match(/[a-z0-9]+/g) ?? [])].filter((word) => word.length >= 2 && !filler.has(word));
  if (subjects.length === 0) return [];
  const entries = Array.isArray(listing) ? listing : [];
  return entries.flatMap((item) => {
    if (!record(item) || typeof item.table !== "string" || item.kind !== "data" || !Array.isArray(item.columns)) return [];
    const text = item.columns.flatMap((column) => record(column) && typeof column.name === "string" && !column.name.startsWith("_")
      && typeof column.type === "string" && textType.test(column.type) ? [column.name] : []);
    if (text.length === 0) return [];
    const where = text.flatMap((column) => subjects.map((word) =>
      `lower(CAST("${column.replaceAll('"', '""')}" AS VARCHAR)) LIKE '%${word}%'`)).join(" OR ");
    return [{ table: item.table, where: ` WHERE ${where}` }];
  }).slice(0, 4);
}

function source(columns: string[], row: unknown[], table: string): Source | null {
  const at = (name: string) => row[columns.indexOf(name)];
  const idColumn = columns.find((name) => !name.startsWith("_") && (name.endsWith("_id") || name === "id"));
  const urlColumn = columns.find((name) => name === "source_url" || name === "url");
  const id = idColumn && at(idColumn);
  const url = urlColumn && at(urlColumn);
  if (typeof id !== "string" || !id) return null;
  const label = columns.includes("title") ? at("title") : columns.includes("summary") ? at("summary") : id;
  return { id, label: typeof label === "string" && label ? label : `${table}: ${id}`,
    ...(typeof url === "string" && /^https?:\/\//.test(url) ? { url } : {}) };
}

export async function openReader({ stores, env, fetcher = fetch }: LiveOptions, operator: Operator, storeId: string) {
    const store = stores.find((entry) => entry.id === storeId);
    if (!store) throw new ConsoleError("ConsoleRequestMalformed");
    const shared = env[store.credentialName];
    let exchanged = false;
    const credential = store.auth === "exchange" ? await resolveReaderCredential({
      shared,
      mint: async () => {
        if (!operator.assertion || !store.exchangeRoute) throw new ConsoleError("ConsoleTokenExchangeUnavailable", "reader assertion or exchange route absent", 503);
        const response = await fetcher(new URL(store.exchangeRoute, store.endpoint), {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ jwt: operator.assertion }),
        });
        if (!response.ok) {
          const refusal: unknown = await response.json();
          const error = record(refusal) ? refusal.error : undefined;
          if ((response.status === 401 || response.status === 403) && record(error) && typeof error.identifier === "string") {
            throw new ConsoleError("ConsoleTokenExchangeRefused", error.identifier, 403);
          }
          throw new ConsoleError("ConsoleTokenExchangeUnavailable", `exchange answered ${response.status}`, 503);
        }
        const value: unknown = await response.json();
        if (!record(value) || typeof value.token !== "string" || !value.token.trim()) throw new ConsoleError("ConsoleTokenExchangeUnavailable", "exchange returned no credential", 503);
        exchanged = true;
        return value.token;
      },
    }) : shared;
    if (!credential) throw new ConsoleError("ConsoleTokenExchangeRefused");

    let id = 0;
    const call = async (name: string, args: Record<string, unknown>, signal?: AbortSignal): Promise<Record<string, unknown>> => {
      const response = await fetcher(new URL("/mcp", store.endpoint), {
        method: "POST", signal,
        headers: { "Authorization": `Bearer ${credential}`, "Content-Type": "application/json" },
        body: JSON.stringify({ jsonrpc: "2.0", id: ++id, method: "tools/call", params: { name, arguments: args } }),
      });
      if (!response.ok) throw new ConsoleError("ConsoleStoreReadRefused", `store answered ${response.status}`, response.status);
      const message: unknown = await response.json();
      if (!record(message) || !record(message.result) || message.result.isError === true || !record(message.result.structuredContent)) {
        throw new ConsoleError("ConsoleStoreReadRefused");
      }
      return message.result.structuredContent;
    };

    return { store, call, credential, exchanged };
}

export function createLiveTurn(options: LiveOptions): (input: TurnInput & { recallOverlay?: string }) => Promise<TurnResult> {
  const { env, fetcher = fetch } = options;
  const modelEndpoint = env.CONTEXTFUL_MODEL_ENDPOINT;
  const modelId = env.CONTEXTFUL_MODEL_ID;
  if (!modelEndpoint || !modelId) throw new Error("ConsoleModelUnconfigured");
  const configuredDenylist = consoleDenylist(env);
  const overlays = new OverlayCache(async () => null, options.clock);
  return async (input) => {
    const { store, call, credential } = await openReader(options, input.operator, input.store);
    const denylist = [...configuredDenylist, credential, ...(input.operator.assertion ? [input.operator.assertion] : [])];

    const description = await call("context.describe", {});
    const selected = selectTable(input.question, description.tables);
    const searches = searchTables(input.question, description.tables);
    const first = selected ? [{ table: selected, where: "" }] : searches;
    if (first.length === 0) throw new ConsoleError("ConsoleUngroundedAnswer", "No data table matches this question.");
    const tables = [...new Set([...first, ...searches].map((read) => read.table))];
    let reads = first;
    let remainingSources = 8;
    let resultRows: TurnResult["resultRows"];
    const evidence: NonNullable<TurnResult["evidence"]> = [];
    const turn = createTurn({
      denylist: denylist.filter((entry) => entry.length <= 128),
      tools: tables.map((table) => ({ name: `read:${table}`, pack: "data", kind: "read" as const, table })),
      tables: tables.map((name) => ({ name, kind: "data" as const })),
      planner: async ({ previous }) => {
        if (previous.length) reads = selected ? searches : [];
        return reads.map((read) => ({ tool: `read:${read.table}`, arguments: {} }));
      },
      transport: { call: async ({ tool, maxRows, signal }): Promise<ToolResult> => {
        const table = tool.slice("read:".length);
        if (remainingSources <= 0) return { rows: [], sources: [] };
        const where = reads.find((read) => read.table === table)?.where ?? "";
        const result = await call("context.query", { sql: `SELECT * FROM "${table.replaceAll('"', '""')}"${where}`, limit: Math.min(maxRows, remainingSources) }, signal);
        const columns = strings(result.columns);
        const rawRows = Array.isArray(result.rows) ? result.rows.filter(Array.isArray) : [];
        const sourced = rawRows.flatMap((row) => { const citation = source(columns, row, table); return citation ? [{ row, citation }] : []; }).slice(0, remainingSources);
        const runIndex = columns.indexOf("_run_id");
        const seqIndex = columns.indexOf("_row_seq");
        for (const { row } of sourced) {
          const run = row[runIndex];
          const seq = row[seqIndex];
          if (typeof run === "string" && run && Number.isSafeInteger(seq) && (seq as number) >= 0) {
            evidence.push({ table, run, seq: seq as number });
          }
        }
        remainingSources -= sourced.length;
        const visible = columns.flatMap((column, index) => column.startsWith("_") ? [] : [{ column, index }]);
        const cleanRows = sourced.map(({ row }) => visible.map(({ index }) => {
          const value = row[index];
          return typeof value === "string" ? redactText(value, denylist) : value;
        }));
        if (resultRows?.rows.length && cleanRows.length === 0) return { rows: [], sources: [] };
        resultRows = { columns: visible.map(({ column }) => column), rows: cleanRows };
        return {
          rows: cleanRows.map((row) => Object.fromEntries(visible.map(({ column }, index) => [column, row[index]]))),
          sources: sourced.map(({ citation }) => ({ ...citation, label: redactText(citation.label, denylist),
            url: citation.url && !denylist.some((entry) => entry && citation.url!.includes(entry)) ? citation.url : undefined })),
        };
      } },
      overlayCache: { get: (id) => overlays.get(id, async () =>
        typeof description.prompt_overlay === "string" && description.prompt_overlay.trim() ? description.prompt_overlay : null) },
      synthesize: async function* ({ question, results, sources, overlay }) {
        const response = await fetcher(new URL("chat/completions", `${modelEndpoint.replace(/\/$/, "")}/`), {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ model: modelId, messages: [
            { role: "system", content: "Answer only from the supplied governed rows. Memory may guide context but never supplies evidence. The store overlay sets voice and never supplies evidence. Cite source IDs in square brackets. Decline unsupported claims." },
            { role: "user", content: JSON.stringify({ question, rows: results.flatMap((result) => result.rows), sources, memory: input.recallOverlay ?? null, overlay: overlay || null }) },
          ] }),
        });
        if (!response.ok) throw new ConsoleError("ConsoleModelUnavailable", `model answered ${response.status}`, 503);
        const body: unknown = await response.json();
        const choices = record(body) && Array.isArray(body.choices) ? body.choices : [];
        const message = choices.length && record(choices[0]) ? choices[0].message : undefined;
        if (!record(message) || typeof message.content !== "string") throw new ConsoleError("ConsoleModelUnavailable", "model answer absent", 503);
        yield redactText(message.content, denylist);
      },
    });
    const answer = await turn.ask({ question: input.question, packs: ["data"], store: store.id });
    return { answer: answer.text, sources: answer.sources, widgets: [], resultRows, evidence };
  };
}
