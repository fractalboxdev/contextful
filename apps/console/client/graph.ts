// The workflow canvas's graph: typed nodes from the store-published workflow, its explicit
// edge list, and a layered layout computed without measuring the DOM, so the prerendered
// first paint and the hydrated page draw the same picture.

export type Run = { status?: string; run_id?: string };
export type Pipeline = { id: string; schedule?: string | null; steps?: string[]; after?: string | null };
export type Job = { name: string; kind: string; schedule?: string | null; after?: string | null; target?: string | null };
export type Model = { id: string; publish?: boolean };
export type Publication = { name: string; job: string; members?: string[] };
export type External = { name: string; label?: string; inputs?: string[]; outputs?: string[] };
export type Edge = { from: string; to: string; via: string };
export type Workflows = {
  applied?: number | null;
  pipelines?: Pipeline[];
  jobs?: Job[];
  models?: Model[];
  publications?: Publication[];
  externals?: External[];
  tables?: string[];
  edges?: Edge[];
  runs?: Record<string, Run>;
};

export type NodeKind = "pipeline" | "table" | "model" | "job" | "publication" | "external";

export type GraphNode = {
  id: string;
  kind: NodeKind;
  name: string;
  label: string;
  /** Whether the engine runs this node; an external step is drawn but never run. */
  engine: boolean;
  detail?: string;
  steps?: string[];
  run?: Run;
  column: number;
  row: number;
};

export type Graph = { nodes: GraphNode[]; edges: Edge[]; columns: number; rows: number };

const KIND_ORDER: NodeKind[] = ["pipeline", "table", "model", "job", "publication", "external"];

/** Typed nodes and the edges joining two of them, laid out in columns by longest path. */
export function workflowGraph(workflows: Workflows): Graph {
  const nodes = new Map<string, Omit<GraphNode, "column" | "row">>();
  const add = (node: Omit<GraphNode, "column" | "row">) => { if (!nodes.has(node.id)) nodes.set(node.id, node); };
  for (const p of workflows.pipelines ?? []) {
    add({ id: `pipeline:${p.id}`, kind: "pipeline", name: p.id, label: p.id, engine: true, detail: p.schedule ?? undefined, steps: p.steps, run: workflows.runs?.[p.id] });
  }
  for (const t of workflows.tables ?? []) add({ id: `table:${t}`, kind: "table", name: t, label: t, engine: true });
  for (const m of workflows.models ?? []) add({ id: `model:${m.id}`, kind: "model", name: m.id, label: m.id, engine: true });
  for (const j of workflows.jobs ?? []) {
    add({ id: `job:${j.name}`, kind: "job", name: j.name, label: j.name, engine: true, detail: j.schedule ?? j.kind, run: workflows.runs?.[`job:${j.name}`] });
  }
  for (const g of workflows.publications ?? []) add({ id: `publication:${g.name}`, kind: "publication", name: g.name, label: g.name, engine: true });
  for (const x of workflows.externals ?? []) {
    add({ id: `external:${x.name}`, kind: "external", name: x.name, label: x.label || x.name, engine: false });
  }
  const edges = (workflows.edges ?? []).filter((e) => nodes.has(e.from) && nodes.has(e.to) && e.from !== e.to);

  // Longest-path layering; a node on a cycle keeps the depth it reached on its first visit.
  const incoming = new Map<string, string[]>();
  for (const e of edges) incoming.set(e.to, [...(incoming.get(e.to) ?? []), e.from]);
  const depth = new Map<string, number>();
  const visiting = new Set<string>();
  const layer = (id: string): number => {
    const known = depth.get(id);
    if (known !== undefined) return known;
    if (visiting.has(id)) return 0;
    visiting.add(id);
    const value = Math.max(-1, ...(incoming.get(id) ?? []).map(layer)) + 1;
    visiting.delete(id);
    depth.set(id, value);
    return value;
  };
  const ordered = [...nodes.values()].sort((a, b) => KIND_ORDER.indexOf(a.kind) - KIND_ORDER.indexOf(b.kind) || a.name.localeCompare(b.name));
  const rowsByColumn = new Map<number, number>();
  const placed: GraphNode[] = ordered.map((node) => {
    const column = layer(node.id);
    const row = rowsByColumn.get(column) ?? 0;
    rowsByColumn.set(column, row + 1);
    return { ...node, column, row };
  });
  return {
    nodes: placed,
    edges,
    columns: Math.max(0, ...placed.map((n) => n.column + 1)),
    rows: Math.max(0, ...rowsByColumn.values()),
  };
}
