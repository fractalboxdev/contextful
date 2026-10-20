import type { Pipeline, Run, Workflows } from "./graph.ts";

export type SourceDraft = { id: string; connector: string; table: string; schedule: string; config: string };

/** The form appends one public declaration to the store's versioned document. */
export function appendSource(document: string, source: SourceDraft): string {
  if (!source.id.trim() || !source.connector.trim() || !source.table.trim()) throw new Error("Enter a source identity, connector and output table.");
  const quote = (value: string) => JSON.stringify(value.trim());
  return `${document.trimEnd()}\n\n[[pipeline]]\nid = ${quote(source.id)}\ntables = [${quote(source.table)}]\n${source.schedule.trim() ? `schedule = ${quote(source.schedule)}\n` : ""}[pipeline.source]\nname = ${quote(source.connector)}\n[pipeline.source.config]\n${source.config.trim()}\n`;
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
  const latest = workflows.run_details?.[source.id] ?? workflows.runs?.[source.id];
  return {
    first: confirmed && confirmed.applied === version ? `${describe(confirmed.first)}${(source.tables?.length ?? 0) > 1 && confirmed.complete !== true ? " · Other output tables are pending confirmation." : ""}` : `Pending — no run confirmed for applied version ${version ?? "unknown"}`,
    latest: latest ? describe(latest) : "No runs recorded",
  };
}
