import { createHash } from "node:crypto";
import type { Operator, TurnInput } from "./index.ts";
import type { Arrival, Conclusion } from "./brief.ts";
import { learnAfterAnswer, type Learning } from "./learn.ts";
import { consoleDenylist, createLiveTurn, openReader, redactText, selectTable, type LiveOptions } from "./live.ts";

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

export function createLiveAnswer(options: AnswerOptions) {
  const { env, fetcher = fetch, clock = Date.now } = options;
  const endpoint = env.CONTEXTFUL_MODEL_ENDPOINT;
  const modelId = env.CONTEXTFUL_MODEL_ID;
  if (!endpoint || !modelId) throw new Error("ConsoleModelUnconfigured");
  const modelUrl = new URL("chat/completions", `${endpoint.replace(/\/$/, "")}/`);
  const configuredDenylist = consoleDenylist(env);
  const liveTurn = createLiveTurn(options);
  const learnings = new Map<string, Map<string, Learning>>();
  const turns = new Map<string, number>();
  const actorScope = (operator: Operator, store: string) => scope(operator.subject, store);
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
        if (!observed(entry.subject, question, answer)) return [];
        return [{ subject: redactText(entry.subject, denied(operator)), key: redactText(entry.key, denied(operator)),
          learning: redactText(entry.learning, denied(operator)) }];
      });
    } catch { return []; }
  }

  async function turn(input: TurnInput) {
    const result = await liveTurn(input);
    const reader = actorScope(input.operator, input.store);
    const session = sessionScope(input.operator, input.store);
    const stream = (async function* () { yield result.answer; })();
    await learnAfterAnswer({ scope: reader, question: input.question, stream,
      distill: ({ question, answer }) => distill(question, answer, input.operator),
      land: async (key, entry) => {
        const found = learnings.get(key) ?? new Map<string, Learning>();
        found.set(`${entry.subject}\0${entry.key}`, entry);
        learnings.set(key, found);
      },
    });
    turns.set(session, (turns.get(session) ?? 0) + 1);
    return result;
  }

  async function brief(operator: Operator, store: string) {
    const entries = [...(learnings.get(actorScope(operator, store))?.values() ?? [])];
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
