import { useEffect, useState } from "react";
import { CheckIcon, FileCode2Icon, LoaderIcon, SaveIcon, WorkflowIcon } from "lucide-react";
import { post, Shell, StoreSelect, TopNav, type Store } from "./shell.tsx";
import { workflowGraph, type GraphNode, type NodeKind, type Workflows } from "./graph.ts";
import { Badge, Button, Textarea, cn } from "./ui.tsx";

type Loaded<T> = { store: string; value: T } | { store: string; error: string };

function runVariant(status: string | undefined) {
  const value = (status ?? "").toLowerCase();
  if (["ok", "success", "succeeded", "completed", "passed"].includes(value)) return "positive" as const;
  if (["failed", "error", "refused"].includes(value)) return "negative" as const;
  if (["running", "pending", "queued"].includes(value)) return "warning" as const;
  return "muted" as const;
}

async function read<T>(query: string, store: string): Promise<Loaded<T>> {
  try {
    const response = await fetch(query + encodeURIComponent(store));
    const value = await response.json() as T & { error?: { identifier?: string } };
    return response.ok ? { store, value } : { store, error: value.error?.identifier ?? "Request failed" };
  } catch (error) {
    return { store, error: error instanceof Error ? error.message : "Request failed" };
  }
}

export function AdminApp() {
  const [stores, setStores] = useState<Store[]>([]);
  const [store, setStore] = useState("");
  const [workflows, setWorkflows] = useState<Loaded<Workflows> | null>(null);
  const [record, setRecord] = useState<Loaded<unknown> | null>(null);
  const [refreshKey, setRefreshKey] = useState(0);

  useEffect(() => {
    fetch("/admin/api/stores").then((response) => response.json() as Promise<Store[]>).catch(() => []).then((list) => {
      const known = Array.isArray(list) ? list : [];
      setStores(known);
      setStore((current) => current || known[0]?.id || "");
    });
  }, []);

  useEffect(() => {
    setWorkflows(null);
    setRecord(null);
    if (!store) return;
    let ignore = false;
    void read<Workflows>("/admin/api/workflows?store=", store).then((loaded) => { if (!ignore) setWorkflows(loaded); });
    void read<unknown>("/admin/api/record?store=", store).then((loaded) => { if (!ignore) setRecord(loaded); });
    return () => { ignore = true; };
  }, [store, refreshKey]);

  const current = workflows && "value" in workflows ? workflows.value : null;
  const applied = current?.applied ?? null;

  return (
    <Shell page="admin" header={
      <TopNav page="admin" title={<span className="font-medium text-foreground">Store operations</span>}>
        <StoreSelect id="admin-store" stores={stores} value={store} onChange={setStore} />
      </TopNav>
    }>
      <main className="h-full overflow-y-auto scrollbar-thin">
        <div className="mx-auto flex w-full max-w-5xl flex-col gap-6 px-4 py-6">
          <div>
            <h1 className="text-xl font-semibold">Store operations</h1>
            <p className="text-sm text-muted-foreground">Pipelines, jobs, models, publications and the steps around them, with run outcomes, come from the store.</p>
          </div>
          <section aria-labelledby="canvas-title" className="flex flex-col gap-3">
            <h2 id="canvas-title" className="flex items-center gap-1.5 text-[11px] font-medium tracking-wider text-muted-foreground uppercase">
              <WorkflowIcon className="size-3" /> Workflow canvas
            </h2>
            <div id="canvas">
              {workflows === null ? <span className="shimmer text-xs font-medium">Reading workflow state…</span> :
                "error" in workflows ? <p className="text-sm text-destructive">{workflows.error}</p> :
                <WorkflowGraph workflows={workflows.value} />}
            </div>
          </section>
          <section aria-labelledby="record-title" className="flex flex-col gap-3">
            <h2 id="record-title" className="flex items-center gap-1.5 text-[11px] font-medium tracking-wider text-muted-foreground uppercase">
              <FileCode2Icon className="size-3" /> Operational record
            </h2>
            <pre id="record" className="max-h-96 overflow-auto rounded-xl border border-border bg-card/40 p-3 font-mono text-xs leading-relaxed text-foreground/90 scrollbar-thin">
              {record === null ? "Reading the record…" : "error" in record ? record.error : JSON.stringify(record.value, null, 2)}
            </pre>
          </section>
          <ControlDocument key={store} store={store} applied={applied} onApplied={() => setRefreshKey((key) => key + 1)} />
        </div>
      </main>
    </Shell>
  );
}

const NODE_W = 184;
const NODE_H = 76;
const GAP_X = 56;
const GAP_Y = 16;

const KIND_LABEL: Record<NodeKind, string> = {
  pipeline: "Pipeline", table: "Table", model: "Model", job: "Job", publication: "Publication", external: "External step",
};

/** The store-published workflow as one node–edge graph; external steps are drawn, never run. */
export function WorkflowGraph({ workflows }: { workflows: Workflows }) {
  const graph = workflowGraph(workflows);
  if (graph.nodes.length === 0) return <p className="text-sm text-muted-foreground">No workflows are published.</p>;
  const at = (node: GraphNode) => ({ x: node.column * (NODE_W + GAP_X), y: node.row * (NODE_H + GAP_Y) });
  const byId = new Map(graph.nodes.map((node) => [node.id, node]));
  const width = graph.columns * (NODE_W + GAP_X) - GAP_X;
  const height = graph.rows * (NODE_H + GAP_Y) - GAP_Y;
  return (
    <div className="overflow-x-auto rounded-xl border border-border bg-card/40 p-4 scrollbar-thin">
      <div className="relative" style={{ width, height }} data-graph-nodes={graph.nodes.length} data-graph-edges={graph.edges.length}>
        <svg className="absolute inset-0 overflow-visible text-muted-foreground" width={width} height={height} aria-hidden>
          <defs>
            <marker id="edge-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="6" markerHeight="6" orient="auto-start-reverse">
              <path d="M0,0 L8,4 L0,8 z" fill="currentColor" />
            </marker>
          </defs>
          {graph.edges.map((edge) => {
            const from = at(byId.get(edge.from)!);
            const to = at(byId.get(edge.to)!);
            const [x1, y1] = [from.x + NODE_W, from.y + NODE_H / 2];
            const [x2, y2] = [to.x, to.y + NODE_H / 2];
            const bend = Math.max(24, (x2 - x1) / 2);
            const external = edge.from.startsWith("external:") || edge.to.startsWith("external:");
            return (
              <path key={`${edge.from}>${edge.to}>${edge.via}`} className="edge" data-from={edge.from} data-to={edge.to} data-via={edge.via}
                d={`M${x1},${y1} C${x1 + bend},${y1} ${x2 - bend},${y2} ${x2},${y2}`} fill="none" stroke="currentColor"
                strokeOpacity={0.6} strokeWidth={1.25} strokeDasharray={external ? "4 3" : undefined} markerEnd="url(#edge-arrow)" />
            );
          })}
        </svg>
        {graph.nodes.map((node) => {
          const { x, y } = at(node);
          return <GraphNodeCard key={node.id} node={node} style={{ left: x, top: y, width: NODE_W, height: NODE_H }} />;
        })}
      </div>
      <ul className="sr-only" aria-label="Dataflow">
        {graph.edges.map((edge) => (
          <li key={`${edge.from}>${edge.to}>${edge.via}`}>{byId.get(edge.from)!.label} {edge.via} {byId.get(edge.to)!.label}</li>
        ))}
      </ul>
    </div>
  );
}

function GraphNodeCard({ node, style }: { node: GraphNode; style: { left: number; top: number; width: number; height: number } }) {
  return (
    <div className={cn("node absolute flex flex-col justify-between gap-1 overflow-hidden rounded-lg border px-2.5 py-2 text-xs",
      node.engine ? "border-border bg-card" : "border-dashed border-muted-foreground/50 bg-transparent")}
      style={style} data-node={node.id} data-kind={node.kind} data-engine={node.engine ? "engine" : "external"}>
      <div className="flex items-center justify-between gap-2">
        <span className="text-[10px] font-medium tracking-wider text-muted-foreground uppercase">{KIND_LABEL[node.kind]}</span>
        {node.detail && <span className="truncate font-mono text-[10px] text-muted-foreground">{node.detail}</span>}
      </div>
      <span className="truncate font-mono text-sm text-foreground" title={node.name}>{node.label}</span>
      {node.engine ? (
        (node.kind === "pipeline" || node.kind === "job") && (
          <div className="flex items-center gap-1.5 text-muted-foreground">
            {node.run ? <Badge variant={runVariant(node.run.status)} className="run">{node.run.status ?? "unknown"}</Badge> : "No runs recorded"}
            {node.steps && node.steps.length > 0 && <span className="truncate font-mono text-[10px]">{node.steps.join(" → ")}</span>}
          </div>
        )
      ) : <span className="text-muted-foreground italic">Runs outside the engine</span>}
    </div>
  );
}

/** Save a TOML draft against the applied version, then apply that draft by its nonce. */
function ControlDocument({ store, applied, onApplied }: { store: string; applied: number | null; onApplied: () => void }) {
  const [text, setText] = useState("");
  const [nonce, setNonce] = useState<string | null>(null);
  const [result, setResult] = useState("");
  const [busy, setBusy] = useState<"edit" | "apply" | null>(null);

  async function run(action: "edit" | "apply") {
    if (applied === null || !Number.isSafeInteger(applied) || !store) {
      setResult("Select a store with an applied version.");
      return;
    }
    if (action === "apply" && !nonce) {
      setResult("Save a draft before applying.");
      return;
    }
    setBusy(action);
    try {
      const body = action === "edit" ? { store, expected: applied, document: text } : { store, expected: applied, nonce };
      const answer = await post<{ nonce?: string }>(`/admin/api/${action}`, body);
      setResult(`${action} complete`);
      if (action === "edit") setNonce(answer.nonce ?? null);
      else {
        setNonce(null);
        onApplied();
      }
    } catch (error) {
      setResult(error instanceof Error ? error.message : "Request failed");
    } finally {
      setBusy(null);
    }
  }

  return (
    <section aria-labelledby="control-title" className="flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <h2 id="control-title" className="flex items-center gap-1.5 text-[11px] font-medium tracking-wider text-muted-foreground uppercase">
          <FileCode2Icon className="size-3" /> Control document
        </h2>
        <p id="version" className="text-xs text-muted-foreground">
          {applied === null ? "No applied version" : <>Applied version <span className="font-mono text-foreground">{applied}</span></>}
          {nonce && <Badge variant="warning" className="ml-2">Draft saved</Badge>}
        </p>
      </div>
      <label htmlFor="document" className="sr-only">TOML document</label>
      <Textarea id="document" value={text} onChange={(event) => { setText(event.target.value); setNonce(null); }} spellCheck={false}
        placeholder={"[pipelines.example]\nschedule = \"every 1h\""} className="min-h-48 font-mono text-xs" />
      <div className="flex items-center gap-2">
        <Button id="edit" type="button" variant="outline" disabled={busy !== null} onClick={() => void run("edit")}>
          {busy === "edit" ? <LoaderIcon className="animate-spin" /> : <SaveIcon />} Save draft
        </Button>
        <Button id="apply" type="button" disabled={busy !== null || !nonce} onClick={() => void run("apply")}>
          {busy === "apply" ? <LoaderIcon className="animate-spin" /> : <CheckIcon />} Apply draft
        </Button>
        <p id="result" role="status" className="text-xs text-muted-foreground">{result}</p>
      </div>
    </section>
  );
}
