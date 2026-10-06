import type { StoreEntry } from "../../gateway/src/index.ts";
import type { TurnInput, TurnResult } from "./index.ts";
import { ConsoleError, createTurn, resolveReaderCredential, type Source, type ToolResult } from "./turn.ts";

type LiveOptions = { stores: StoreEntry[]; env: NodeJS.ProcessEnv; fetcher?: typeof fetch };

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function strings(value: unknown): string[] {
  return Array.isArray(value) ? value.filter((item): item is string => typeof item === "string") : [];
}

function source(columns: string[], row: unknown[], table: string): Source | null {
  const at = (name: string) => row[columns.indexOf(name)];
  const idColumn = columns.find((name) => name.endsWith("_id") || name === "id");
  const urlColumn = columns.find((name) => name === "source_url" || name === "url");
  const id = idColumn && at(idColumn);
  const url = urlColumn && at(urlColumn);
  if (typeof id !== "string" || !id) return null;
  const label = columns.includes("title") ? at("title") : columns.includes("summary") ? at("summary") : id;
  return { id, label: typeof label === "string" && label ? label : `${table}: ${id}`,
    ...(typeof url === "string" && /^https?:\/\//.test(url) ? { url } : {}) };
}

export function createLiveTurn({ stores, env, fetcher = fetch }: LiveOptions): (input: TurnInput) => Promise<TurnResult> {
  const modelEndpoint = env.CONTEXTFUL_MODEL_ENDPOINT;
  const modelId = env.CONTEXTFUL_MODEL_ID;
  if (!modelEndpoint || !modelId) throw new Error("ConsoleModelUnconfigured");
  return async (input) => {
    const store = stores.find((entry) => entry.id === input.store);
    if (!store) throw new ConsoleError("ConsoleRequestMalformed");
    const shared = env[store.credentialName];
    const credential = store.auth === "exchange" ? await resolveReaderCredential({
      shared,
      mint: async () => {
        if (!input.operator.assertion || !store.exchangeRoute) throw new ConsoleError("ConsoleTokenExchangeRefused");
        const response = await fetcher(new URL(store.exchangeRoute, store.endpoint), {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ jwt: input.operator.assertion }),
        });
        if (!response.ok) throw new ConsoleError("ConsoleTokenExchangeRefused");
        const value: unknown = await response.json();
        if (!record(value) || typeof value.token !== "string") throw new ConsoleError("ConsoleTokenExchangeRefused");
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

    const description = await call("context.describe", {});
    const tables = Array.isArray(description.tables) ? description.tables.flatMap((item) =>
      record(item) && typeof item.table === "string" && item.kind === "data" ? [item.table] : []) : [];
    const turn = createTurn({
      tools: tables.map((table) => ({ name: `read:${table}`, pack: "data", kind: "read" as const, table })),
      tables: tables.map((name) => ({ name, kind: "data" as const })),
      planner: async ({ previous }) => previous.length ? [] : tables.map((table) => ({ tool: `read:${table}`, arguments: {} })),
      transport: { call: async ({ tool, maxRows, signal }): Promise<ToolResult> => {
        const table = tool.slice("read:".length);
        const result = await call("context.query", { sql: `SELECT * FROM "${table.replaceAll('"', '""')}"`, limit: maxRows }, signal);
        const columns = strings(result.columns);
        const rawRows = Array.isArray(result.rows) ? result.rows.filter(Array.isArray) : [];
        const sourced = rawRows.flatMap((row) => { const citation = source(columns, row, table); return citation ? [{ row, citation }] : []; });
        return {
          rows: sourced.map(({ row }) => Object.fromEntries(columns.map((column, index) => [column, row[index]]))),
          sources: sourced.map(({ citation }) => citation),
        };
      } },
      synthesize: async function* ({ question, results, sources }) {
        const response = await fetcher(new URL("chat/completions", `${modelEndpoint.replace(/\/$/, "")}/`), {
          method: "POST", headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ model: modelId, messages: [
            { role: "system", content: "Answer only from the supplied governed rows. Cite source IDs in square brackets. Decline unsupported claims." },
            { role: "user", content: JSON.stringify({ question, rows: results.flatMap((result) => result.rows), sources }) },
          ] }),
        });
        if (!response.ok) throw new ConsoleError("ConsoleModelUnavailable", `model answered ${response.status}`, 503);
        const body: unknown = await response.json();
        const choices = record(body) && Array.isArray(body.choices) ? body.choices : [];
        const message = choices.length && record(choices[0]) ? choices[0].message : undefined;
        if (!record(message) || typeof message.content !== "string") throw new ConsoleError("ConsoleModelUnavailable", "model answer absent", 503);
        yield message.content;
      },
    });
    const answer = await turn.ask({ question: input.question, packs: ["data"], store: store.id });
    return { answer: answer.text, sources: answer.sources, widgets: [] };
  };
}
