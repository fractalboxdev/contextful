import { searchTables, selectTable } from "./live.ts";
import { ConsoleError } from "./turn.ts";

export const chipPrompt = (label: string) => `Tell me about ${label}`;
export const briefPrompt = (subject: string) => `What changed for ${subject}?`;

function record(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

// A listed table declines when its columns are known and carry no id column, or its row count is zero;
// unknown columns or counts leave the decision to the turn, which reads the table's rows.
function citable(entry: Record<string, unknown> | undefined, rowCount: number | undefined): boolean {
  if (rowCount === 0) return false;
  if (!entry || !Array.isArray(entry.columns)) return true;
  return entry.columns.some((column) => record(column) && typeof column.name === "string" && !column.name.startsWith("_")
    && (column.name === "id" || column.name.endsWith("_id")));
}

/**
 * Applies the live turn's first-round table resolution to a suggested prompt: the table `selectTable`
 * names, else the tables `searchTables` returns. Raises `ConsoleSuggestionUnanswerable` when each declines.
 */
export function assertSuggestion(prompt: string, listing: unknown, rowCounts: Record<string, number | undefined> = {}): void {
  const entries = (Array.isArray(listing) ? listing : []).filter(record);
  const selected = selectTable(prompt, entries);
  const candidates = selected ? [selected] : searchTables(prompt, entries).map((read) => read.table);
  const answerable = candidates.some((table) => citable(entries.find((entry) => entry.table === table), rowCounts[table]));
  if (!answerable) throw new ConsoleError("ConsoleSuggestionUnanswerable", prompt);
}

export function isUnanswerable(error: unknown): boolean {
  return error instanceof ConsoleError && error.code === "ConsoleSuggestionUnanswerable";
}
