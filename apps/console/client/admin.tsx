import { useEffect, useState } from "react";
import { CheckIcon, FileCode2Icon, LoaderIcon, SaveIcon, WorkflowIcon } from "lucide-react";
import { post, RequestFailure, Shell, StoreSelect, TopNav, type Store } from "./shell.tsx";
import { workflowGraph, type GraphNode, type NodeKind, type Workflows } from "./graph.ts";
import { Badge, Button, Textarea, cn } from "./ui.tsx";
import { appendSource, sourceOutcome, type SourceDraft } from "./sources.ts";

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
    setWorkflows((current) => current?.store === store ? current : null);
    setRecord((current) => current?.store === store ? current : null);
    if (!store) return;
    let ignore = false;
    void read<Workflows>("/admin/api/workflows?store=", store).then((loaded) => { if (!ignore) setWorkflows(loaded); });
    void read<unknown>("/admin/api/record?store=", store).then((loaded) => { if (!ignore) setRecord(loaded); });
    return () => { ignore = true; };
  }, [store, refreshKey]);

  const current = workflows && "value" in workflows ? workflows.value : null;
  const applied = current?.applied ?? null;

  useEffect(() => {
    if (!current || applied === null) return;
    const timer = setInterval(() => setRefreshKey((key) => key + 1), 5000);
    return () => clearInterval(timer);
  }, [store, applied]);

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
          <ControlDocument key={store} store={store} applied={applied} workflows={current} onApplied={() => setRefreshKey((key) => key + 1)} />
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
function ControlDocument({ store, applied, workflows, onApplied }: { store: string; applied: number | null; workflows: Workflows | null; onApplied: () => void }) {
  const [text, setText] = useState("");
  const [nonce, setNonce] = useState<string | null>(null);
  const [result, setResult] = useState("");
  const [busy, setBusy] = useState<"edit" | "apply" | null>(null);
  const [dirty, setDirty] = useState(false);
  const [base, setBase] = useState<number | null>(null);
  const [field, setField] = useState<string | undefined>();
  const [source, setSource] = useState<SourceDraft>({ id: "", connector: "http", table: "", schedule: "every 1h", config: 'endpoint = "https://example.com/data"' });

  useEffect(() => {
    if (!dirty && workflows?.document !== undefined) { setText(workflows.document); setBase(applied); }
  }, [workflows?.document, applied, dirty]);

  useEffect(() => {
    if (dirty && base !== applied) { setNonce(null); setResult("The applied version changed. Reload the control document and validate your draft again."); }
  }, [applied, base, dirty]);

  function change(value: string) { setText(value); setDirty(true); setNonce(null); setResult(""); setField(undefined); }

  function addSource() {
    try { change(appendSource(text, source)); setResult("Source added to the draft. Validate and save before applying."); }
    catch (error) { setResult(error instanceof Error ? error.message : "Check the source fields."); }
  }

  async function run(action: "edit" | "apply") {
    if (applied === null || !Number.isSafeInteger(applied) || !store || workflows?.document === undefined) {
      setResult("Select a store with an applied version.");
      return;
    }
    if (base !== applied) { setResult("The applied version changed. Reload the control document and validate your draft again."); return; }
    if (action === "apply" && !nonce) {
      setResult("Save a draft before applying.");
      return;
    }
    setBusy(action);
    try {
      const body = action === "edit" ? { store, expected: applied, document: text } : { store, expected: applied, nonce };
      const answer = await post<{ nonce?: string; applied?: number }>(`/admin/api/${action}`, body);
      if (action === "edit") { setNonce(answer.nonce ?? null); setResult("Draft validated and saved. The applied version is unchanged."); }
      else {
        setNonce(null);
        setDirty(false);
        setResult(`Applied version ${answer.applied ?? "unknown"} claimed. First-run confirmation appears below; applying starts no run.`);
        onApplied();
      }
    } catch (error) {
      setField(error instanceof RequestFailure ? error.field : undefined);
      setResult(error instanceof Error ? error.message : "Request failed");
    } finally {
      setBusy(null);
    }
  }

  return (
    <>
    <section aria-labelledby="sources-title" className="flex flex-col gap-3">
      <h2 id="sources-title" className="text-sm font-semibold">Ingest sources</h2>
      <p className="text-xs text-muted-foreground">Applied sources come from the store control document. A claimed version waits for its schedule or trigger; recorded table runs confirm ingestion.</p>
      {workflows ? <SourceList workflows={workflows} /> : <p className="text-xs text-muted-foreground">Reading applied sources…</p>}
      <fieldset disabled={busy !== null || workflows?.document === undefined} className="grid gap-3 rounded-xl border border-border p-4 md:grid-cols-2">
        <legend className="px-1 text-sm font-medium">Add an ingest source</legend>
        {([
          ["id", "Source identity"], ["connector", "Connector"], ["table", "Output table"], ["schedule", "Schedule (empty for a trigger)"],
        ] as const).map(([key, label]) => <label key={key} htmlFor={`source-${key}`} className="flex flex-col gap-1 text-xs">{label}
          <input id={`source-${key}`} value={source[key]} list={key === "connector" ? "source-connectors" : undefined}
            aria-invalid={field === ({ id: "identity", connector: "source", table: "destination", schedule: "schedule" } as const)[key] || undefined}
            aria-describedby={field === ({ id: "identity", connector: "source", table: "destination", schedule: "schedule" } as const)[key] ? "result" : undefined}
            onChange={(event) => setSource((current) => ({ ...current, [key]: event.target.value }))} className="rounded-lg border border-input bg-transparent p-2 text-sm" />
        </label>)}
        <datalist id="source-connectors">{workflows?.connectors?.map((connector) => <option key={connector.name} value={connector.name}>{connector.available ? "Available" : "Unavailable in this build"}</option>)}</datalist>
        <p className="text-xs text-muted-foreground md:col-span-2">Destination: this store. Use an available native connector or a registered component reference. The store validates connector fields and table compatibility.</p>
        <label htmlFor="source-config" className="flex flex-col gap-1 text-xs md:col-span-2">Connector configuration (TOML; credential references only)
          <Textarea id="source-config" value={source.config} aria-invalid={field === "source" || undefined} aria-describedby={field === "source" ? "result" : undefined} onChange={(event) => setSource((current) => ({ ...current, config: event.target.value }))} className="font-mono text-xs" spellCheck={false} />
        </label>
        <Button type="button" variant="outline" onClick={addSource}>Add source to draft</Button>
      </fieldset>
    </section>
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
      <label htmlFor="document" className="text-xs text-muted-foreground">TOML document — advanced editing</label>
      <Textarea id="document" value={text} aria-invalid={field !== undefined || undefined} aria-describedby="result" onChange={(event) => change(event.target.value)} spellCheck={false}
        placeholder={"[pipelines.example]\nschedule = \"every 1h\""} className="min-h-48 font-mono text-xs" />
      <div className="flex items-center gap-2">
        <Button id="edit" type="button" variant="outline" disabled={busy !== null || workflows?.document === undefined || base !== applied} onClick={() => void run("edit")}>
          {busy === "edit" ? <LoaderIcon className="animate-spin" /> : <SaveIcon />} Validate and save draft
        </Button>
        <Button id="apply" type="button" disabled={busy !== null || !nonce} onClick={() => void run("apply")}>
          {busy === "apply" ? <LoaderIcon className="animate-spin" /> : <CheckIcon />} Apply draft
        </Button>
        {dirty && base !== applied && <Button type="button" variant="outline" onClick={() => { setNonce(null); setDirty(false); setResult("Applied document reloaded. Add the source and validate the draft again."); }}>Reload applied document</Button>}
        <p id="result" role="status" className="text-xs text-muted-foreground">{result}</p>
      </div>
    </section>
    </>
  );
}

export function SourceList({ workflows }: { workflows: Workflows }) {
  if (!workflows.pipelines?.length) return <p className="text-xs text-muted-foreground">No ingest sources are applied.</p>;
  return <div className="overflow-x-auto rounded-xl border border-border"><table className="w-full text-left text-xs">
    <thead><tr>{["Source", "Connector / destination", "Schedule or trigger", "First confirmed table run of applied version", "Latest table outcome"].map((label) => <th key={label} className="p-3 font-medium">{label}</th>)}</tr></thead>
    <tbody>{workflows.pipelines.map((pipeline) => {
      const outcome = sourceOutcome(workflows, pipeline);
      return <tr key={pipeline.id} className="border-t border-border" data-source={pipeline.id}>
        <th scope="row" className="p-3 font-mono font-normal">{pipeline.id}</th>
        <td className="p-3">{pipeline.source ?? "Unknown connector"} → {pipeline.destination ?? "store"}<br />{(pipeline.destinations ?? pipeline.tables)?.join(", ")}</td>
        <td className="p-3">{pipeline.schedule ?? (pipeline.after ? `After ${pipeline.after}` : "External trigger")}</td>
        <td className="p-3" role="status">{outcome.first}</td><td className="p-3">{outcome.latest}</td>
      </tr>;
    })}</tbody>
  </table></div>;
}
