import type { Pipeline, Run, Workflows } from "./graph.ts";

export type SourceDraft = { id: string; connector: string; table: string; schedule: string; config: string };

/** The form appends one public declaration to the store's versioned document. */
export function appendSource(document: string, source: SourceDraft): string {
  if (!source.id.trim() || !source.connector.trim() || !source.table.trim()) throw new Error("Enter a source identity, connector and output table.");
  const quote = (value: string) => JSON.stringify(value.trim());
  return `${document.trimEnd()}\n\n[[pipeline]]\nid = ${quote(source.id)}\ntables = [${quote(source.table)}]\n${source.schedule.trim() ? `schedule = ${quote(source.schedule)}\n` : ""}[pipeline.source]\nname = ${quote(source.connector)}\n[pipeline.source.config]\n${source.config.trim()}\n`;
}

/** The control draft the form and the raw editor share: the document text and, while the form's
 * declaration is in it, the document that declaration was appended to. */
export type Draft = { text: string; pending: string | null };

export type DraftEvent =
  | { type: "add"; source: SourceDraft }
  | { type: "field"; source: SourceDraft }
  | { type: "replace"; text: string };

/** A pending declaration renders from the document it was added to, so a later field change
 * replaces it instead of appending a second one; a raw edit or a reload of the applied document
 * ends the binding. Incomplete fields render the base document alone. */
export function reduceDraft(draft: Draft, event: DraftEvent): Draft {
  switch (event.type) {
    case "add": {
      const base = draft.pending ?? draft.text;
      return { text: appendSource(base, event.source), pending: base };
    }
    case "field": {
      if (draft.pending === null) return draft;
      try { return { text: appendSource(draft.pending, event.source), pending: draft.pending }; }
      catch { return { text: draft.pending, pending: draft.pending }; }
    }
    case "replace":
      return { text: event.text, pending: null };
  }
}

export function sourceOutcome(workflows: Workflows, source: Pipeline): { first: string; latest: string } {
  const failure: Record<string, string> = {
    Transient: "Check the source connection; the run can retry.", Permanent: "Check the source endpoint and request settings before retrying.",
    SchemaIncompatible: "Update the output table schema to accept the source rows.", AuthExpired: "Renew the source credential reference.",
    RateLimited: "Wait for the source request limit to clear.", Config: "Check the connector configuration.", Storage: "Check the store connection and available space.",
    UnknownConnector: "Restore the registered connector build.", SecretNotFound: "Configure the referenced credential.", Canceled: "The run was stopped. Check its operational record before retrying.",
  };
  const describe = (run: Run) => `${run.table ? `Table ${run.table}: ` : ""}${run.status ?? "unknown"}${run.rows === undefined ? "" : ` · ${run.rows} rows`}${run.failure ? ` · ${failure[run.failure] ?? "Check the operational record before retrying."}` : ""}${run.run_id ? ` · ${run.run_id}` : ""}`;
  const confirmed = workflows.source_runs?.[source.id];
  const version = workflows.applied;
  const detail = workflows.run_details?.[source.id];
  const mark = workflows.runs?.[source.id];
  // The local detail belongs to the newest run alone; a newer mark replicated from another node outranks an older local row.
  const latest = detail && (mark?.run_id === undefined || mark.run_id === detail.run_id) ? detail : mark ?? detail;
  return {
    first: confirmed && confirmed.applied === version ? `${describe(confirmed.first)}${(source.tables?.length ?? 0) > 1 && confirmed.complete !== true ? " · Other output tables are pending confirmation." : ""}` : `Pending — no run confirmed for applied version ${version ?? "unknown"}`,
    latest: latest ? describe(latest) : "No runs recorded",
  };
}
