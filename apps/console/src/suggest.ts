import { searchTables, selectTable } from "./live.ts";
import { ConsoleError } from "./turn.ts";

export const chipPrompt = (label: string) => `Tell me about ${label}`;
export const briefPrompt = (subject: string) => `What changed for ${subject}?`;

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

// A table declines when its own describe shows zero rows, or columns with no id column; the engine's
// top-level listing carries neither, so unknown facts leave the decision to the turn, which reads the rows.
function citable(rowCount: number | undefined, columns: unknown): boolean {
  if (rowCount === 0) return false;
  if (!Array.isArray(columns)) return true;
  return columns.some((column) => record(column) && typeof column.name === "string" && !column.name.startsWith("_")
    && (column.name === "id" || column.name.endsWith("_id")));
}

/** The tables the live turn's first round reads for a prompt: the one `selectTable` names, else those `searchTables` returns. */
export function suggestionCandidates(prompt: string, listing: unknown): string[] {
  const entries = (Array.isArray(listing) ? listing : []).filter(record);
  const selected = selectTable(prompt, entries);
  return selected ? [selected] : searchTables(prompt, entries).map((read) => read.table);
}

/**
 * Applies the live turn's first-round table resolution to a suggested prompt over the bare listing, then judges
 * each candidate by its own describe's row count and columns. Raises `ConsoleSuggestionUnanswerable` when each declines.
 */
export function assertSuggestion(prompt: string, listing: unknown, rowCounts: Record<string, number | undefined> = {},
  columnsByTable: Record<string, unknown> = {}): void {
  const answerable = suggestionCandidates(prompt, listing).some((table) => citable(rowCounts[table], columnsByTable[table]));
  if (!answerable) throw new ConsoleError("ConsoleSuggestionUnanswerable", prompt);
}

type Call = (name: string, args: Record<string, unknown>) => Promise<Record<string, unknown>>;

/** A suggestion check over one store reader: one listing read, then one describe per candidate table, each read once. */
export function createSuggestionCheck(call: Call): (prompt: string) => Promise<void> {
  let listing: Promise<unknown> | undefined;
  const described = new Map<string, Promise<Record<string, unknown>>>();
  return async (prompt) => {
    listing ??= call("context.describe", {}).then((value) => value.tables);
    const tables = await listing;
    const candidates = suggestionCandidates(prompt, tables);
    const facts = await Promise.all(candidates.map((table) => {
      if (!described.has(table)) described.set(table, call("context.describe", { table }).catch(() => ({})));
      return described.get(table)!;
    }));
    const rowCounts = Object.fromEntries(candidates.map((table, index) =>
      [table, typeof facts[index].row_count === "number" ? facts[index].row_count as number : undefined]));
    const columns = Object.fromEntries(candidates.map((table, index) => [table, facts[index].columns]));
    assertSuggestion(prompt, tables, rowCounts, columns);
  };
}

export function isUnanswerable(error: unknown): boolean {
  return error instanceof ConsoleError && error.code === "ConsoleSuggestionUnanswerable";
}
