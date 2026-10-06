import { createHash } from "node:crypto";
import type { Operator, TurnInput } from "./index.ts";
import type { Arrival, Conclusion } from "./brief.ts";
import { learnAfterAnswer, type Learning } from "./learn.ts";
import { consoleDenylist, createLiveTurn, openReader, redactText, selectTable, type LiveOptions } from "./live.ts";
import { ConsoleError } from "./turn.ts";

type AnswerOptions = LiveOptions & { clock?: () => number };

function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function scope(...parts: string[]): string {
  return createHash("sha256").update(JSON.stringify(parts)).digest("hex");
}

function observed(subject: string, question: string, answer: string): boolean {
  const word = subject.trim().toLowerCase();
  return word.length >= 3 && (`${question} ${answer}`).toLowerCase().includes(word);
}

function observedLearning(learning: string, answer: string): boolean {
  const statement = learning.trim().replace(/\s+/g, " ").toLowerCase();
  return statement.length >= 3 && answer.replace(/\s+/g, " ").toLowerCase().includes(statement);
}

function memoryTable(description: unknown): string | null {
  const tables = object(description) && Array.isArray(description.tables) ? description.tables : [];
  const memory = tables.flatMap((table) => object(table) && table.kind === "memory" && typeof table.table === "string" ? [table.table] : []);
  return memory.length === 1 ? memory[0] : null;
}

function rows(value: unknown): { columns: string[]; rows: unknown[][] } {
  if (!object(value)) return { columns: [], rows: [] };
  return { columns: Array.isArray(value.columns) ? value.columns.filter((column): column is string => typeof column === "string") : [],
    rows: Array.isArray(value.rows) ? value.rows.filter(Array.isArray) : [] };
}

function matchesEntity(question: string, subject: string): boolean {
  const asked = new Set(question.toLowerCase().match(/[a-z0-9]{3,}/g) ?? []);
  return (subject.toLowerCase().match(/[a-z0-9]{3,}/g) ?? []).some((part) => asked.has(part));
}

function namedSubjects(question: string): string[] {
  const stop = new Set(["Which", "What", "Who", "When", "Where", "Why", "How", "The"]);
  return (question.match(/\b[A-Z][A-Za-z0-9]{2,}\b/g) ?? []).filter((name) => !stop.has(name));
}

export function createLiveAnswer(options: AnswerOptions) {
  const { env, fetcher = fetch, clock = Date.now } = options;
  const endpoint = env.CONTEXTFUL_MODEL_ENDPOINT;
  const modelId = env.CONTEXTFUL_MODEL_ID;
  if (!endpoint || !modelId) throw new Error("ConsoleModelUnconfigured");
  const modelUrl = new URL("chat/completions", `${endpoint.replace(/\/$/, "")}/`);
  const configuredDenylist = consoleDenylist(env);
  const liveTurn = createLiveTurn(options);
  const turns = new Map<string, number>();
  const sessionScope = (operator: Operator, store: string) => scope(operator.subject, store, operator.assertion ?? "");
  const denied = (operator: Operator) => [...configuredDenylist, ...(operator.assertion ? [operator.assertion] : [])];

  async function distill(question: string, answer: string, operator: Operator): Promise<Learning[]> {
    try {
      const response = await fetcher(modelUrl, {
        method: "POST", headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ model: modelId, messages: [
          { role: "system", content: "Distil at most three observed facts into JSON entries with subject, key, learning. Only subjects present in the question or answer qualify." },
          { role: "user", content: JSON.stringify({ question, answer }) },
        ] }),
      });
      if (!response.ok) return [];
      const body: unknown = await response.json();
      const choices = object(body) && Array.isArray(body.choices) ? body.choices : [];
      const message = choices.length && object(choices[0]) ? choices[0].message : undefined;
      if (!object(message) || typeof message.content !== "string") return [];
      const parsed: unknown = JSON.parse(message.content);
      const entries = object(parsed) && Array.isArray(parsed.entries) ? parsed.entries : [];
      return entries.flatMap((entry): Learning[] => {
        if (!object(entry) || typeof entry.subject !== "string" || typeof entry.key !== "string" || typeof entry.learning !== "string") return [];
        if (!observed(entry.subject, question, answer) || !observedLearning(entry.learning, answer)) return [];
        return [{ subject: redactText(entry.subject, denied(operator)), key: redactText(entry.key, denied(operator)),
          learning: redactText(entry.learning, denied(operator)) }];
      });
    } catch { return []; }
  }

  async function readMemory(operator: Operator, storeId: string, question?: string) {
    if (!operator.session || !operator.assertion) return null;
    const reader = await openReader(options, operator, storeId);
    if (!reader.exchanged) return null;
    const table = memoryTable(await reader.call("context.describe", {}));
    if (!table) return null;
    const expectedScope = `console:${operator.subject}:${operator.session}`;
    let names = { columns: [] as string[], rows: [] as unknown[][] };
    try {
      names = rows(await reader.call("context.query", {
        sql: `SELECT DISTINCT subject FROM "${table.replaceAll('"', '""')}" LIMIT 100`, limit: 100,
      }));
    } catch (error) {
      if (!(error instanceof ConsoleError && error.code === "ConsoleStoreReadRefused" && error.status === 400)) throw error;
    }
    const subjectAt = names.columns.indexOf("subject");
    const known = names.rows.map((row) => row[subjectAt]).filter((value): value is string =>
      typeof value === "string" && (question === undefined || matchesEntity(question, value)));
    const subjects = [...new Set([...known, ...(question === undefined ? [] : namedSubjects(question))])].slice(0, 3);
    const entries: Learning[] = [];
    for (const subject of subjects) {
      const response = rows(await reader.call("memory.recall", { table, subject, limit: 3 }));
      const index = (name: string) => response.columns.indexOf(name);
      for (const row of response.rows) {
        if (row[index("scope")] !== expectedScope) continue;
        const predicate = row[index("predicate")];
        const value = row[index("object")];
        if (typeof predicate === "string" && typeof value === "string") entries.push({ subject, key: predicate, learning: value });
      }
    }
    return { table, credential: reader.credential, scope: expectedScope, entries };
  }

  async function turn(input: TurnInput) {
    const memory = await readMemory(input.operator, input.store, input.question);
    const recallOverlay = (memory?.entries ?? []).map((entry) =>
      redactText(`${entry.subject} ${entry.key}: ${entry.learning}`, denied(input.operator))).join("\n").slice(0, 8000);
    const result = await liveTurn({ ...input, recallOverlay });
    const session = sessionScope(input.operator, input.store);
    if (memory && result.evidence?.length) {
      const stream = (async function* () { yield result.answer; })();
      await learnAfterAnswer({ scope: memory.scope, question: input.question, stream,
        distill: ({ question, answer }) => distill(question, answer, input.operator),
        land: async (_key, entry) => {
          const claim = { subject: entry.subject, predicate: entry.key, object: entry.learning,
            confidence: 0.8, evidence: result.evidence };
          const dedupKey = scope(input.operator.subject, input.operator.session ?? "", input.question, result.answer,
            entry.subject, entry.key, entry.learning);
          let response: Response;
          try {
            response = await fetcher(new URL("/memory/claims", options.stores.find((store) => store.id === input.store)!.endpoint), {
              method: "POST", headers: { Authorization: `Bearer ${memory.credential}`, "Content-Type": "application/json" },
              body: JSON.stringify({ into: memory.table, actor: input.operator.subject, session: input.operator.session,
                dedup_key: dedupKey, claim }),
            });
          } catch { throw new ConsoleError("ConsoleLearningWriteRefused", "memory write unavailable", 503); }
          if (!response.ok) throw new ConsoleError("ConsoleLearningWriteRefused", "memory write refused", 503);
          let receipt: unknown;
          try { receipt = await response.json(); }
          catch { throw new ConsoleError("ConsoleLearningWriteRefused", "memory receipt malformed", 503); }
          if (!object(receipt) || receipt.scope !== memory.scope || typeof receipt.landed !== "boolean") {
            throw new ConsoleError("ConsoleLearningWriteRefused", "memory receipt unbound", 503);
          }
        },
      });
    }
    turns.set(session, (turns.get(session) ?? 0) + 1);
    return result;
  }

  async function brief(operator: Operator, store: string) {
    const durable = await readMemory(operator, store);
    const entries = durable?.entries ?? [];
    const conclusions: Conclusion[] = entries.map((entry) => ({ subject: entry.subject, text: entry.learning, live: true }));
    return {
      session: { turns: turns.get(sessionScope(operator, store)) ?? 0, vantage: "present" },
      conclusions,
      now: clock(),
      loadArrivals: async (): Promise<Arrival[]> => {
        const { call } = await openReader(options, operator, store);
        const description = await call("context.describe", {});
        const question = entries.map((entry) => `${entry.subject} ${entry.key} ${entry.learning}`).join(" ");
        const table = selectTable(question, description.tables);
        if (!table) return [];
        const result = await call("context.query", { sql: `SELECT * FROM "${table.replaceAll('"', '""')}"`, limit: 20 });
        const columns = Array.isArray(result.columns) ? result.columns.filter((value): value is string => typeof value === "string") : [];
        const rows = Array.isArray(result.rows) ? result.rows.filter(Array.isArray) : [];
        const at = (row: unknown[], names: string[]) => names.map((name) => row[columns.indexOf(name)]).find((value) => typeof value === "string" && value) as string | undefined;
        return rows.flatMap((row): Arrival[] => {
          const id = at(row, ["filing_id", "id", "document_id"]);
          const label = at(row, ["title", "label", "summary"]);
          const arrivedAt = at(row, ["published_at", "arrived_at", "date"]);
          if (!id || !label || !arrivedAt) return [];
          const topics = [label, at(row, ["summary"]) ?? ""].map((value) => redactText(value, denied(operator)));
          return [{ id: redactText(id, denied(operator)), label: redactText(label, denied(operator)), topics, arrivedAt }];
        }).slice(0, 20);
      },
    };
  }

  return { turn, brief, briefBudgetMs: 250, redactView: (operator: Operator, value: string) => redactText(value, denied(operator)) };
}
