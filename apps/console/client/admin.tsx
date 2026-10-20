import { useEffect, useState } from "react";
import { CheckIcon, FileCode2Icon, LoaderIcon, SaveIcon, WorkflowIcon } from "lucide-react";
import { post, Shell, StoreSelect, TopNav, type Store } from "./shell.tsx";
import { workflowGraph, type GraphNode, type IngestSource, type NodeKind, type Workflows } from "./graph.ts";
import { Badge, Button, Textarea, cn } from "./ui.tsx";

type Loaded<T> = { store: string; value: T } | { store: string; error: string };

function runVariant(status: string | undefined) {
  const value = (status ?? "").toLowerCase();
  if (["ok", "success", "succeeded", "completed", "passed"].includes(value)) return "positive" as const;
  if (["failed", "error", "refused"].includes(value)) return "negative" as const;
  if (["running", "pending", "queued"].includes(value)) return "warning" as const;
  return "muted" as const;
}

async function read<T>(query: string, store: string, signal?: AbortSignal): Promise<Loaded<T>> {
  try {
    const response = await fetch(query + encodeURIComponent(store), { signal: signal ? AbortSignal.any([signal, AbortSignal.timeout(30_000)]) : AbortSignal.timeout(30_000) });
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
  const pending = current?.sources?.some((source) => ["pending", "running", "waiting"].includes(source.state)) ?? false;

  useEffect(() => {
    if (!store || !pending) return;
    let ignore = false;
    const controller = new AbortController();
    let timer: ReturnType<typeof setTimeout>;
    async function poll() {
      const loaded = await read<Workflows>("/admin/api/workflows?store=", store, controller.signal);
      if (!ignore) { setWorkflows(loaded); timer = setTimeout(() => void poll(), 5000); }
    }
    timer = setTimeout(() => void poll(), 5000);
    return () => { ignore = true; controller.abort(); clearTimeout(timer); };
  }, [store, pending]);

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
          <section aria-labelledby="sources-title" className="flex flex-col gap-3">
            <div className="flex items-center justify-between">
              <h2 id="sources-title" className="text-sm font-medium">Ingest sources</h2>
              <Button type="button" variant="outline" onClick={() => setRefreshKey((key) => key + 1)}>Refresh outcomes</Button>
            </div>
            <SourceList sources={current?.sources ?? []} />
          </section>
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
          <ControlDocument key={store} store={store} applied={applied} document={current?.document ?? ""} onApplied={() => setRefreshKey((key) => key + 1)} />
        </div>
      </main>
    </Shell>
  );
}

export function SourceList({ sources }: { sources: IngestSource[] }) {
  if (sources.length === 0) return <p className="text-sm text-muted-foreground">No ingest sources are applied.</p>;
  return <ul className="flex flex-col gap-3">{sources.map((source) => {
    const latest = source.latest_run;
    return <li key={source.id} className="rounded-xl border border-border bg-card/40 p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2"><strong>{source.id}</strong><Badge variant={runVariant(source.state)}>{source.state}</Badge><span className="text-muted-foreground">Applied version {source.version}</span></div>
      <p className="mt-1 font-mono text-xs">{source.connector} → {source.destinations.join(", ")}</p>
      <p className="mt-1 text-xs text-muted-foreground">{source.schedule ?? (source.after ? `After ${source.after}` : source.trigger === "receive" ? "Triggered by incoming rows" : "Manual trigger")}</p>
      <p className="mt-2 text-xs">{source.first_run ? `First run ${source.first_run.run_id}: ${source.first_run.status}, ${source.first_run.rows} rows` : "Waiting for the first run of this applied version. Applying a source does not start a run."}</p>
      {source.first_run?.message && <p className="mt-1 text-xs text-destructive">First run: {source.first_run.message}</p>}
      {latest && <p className="mt-1 text-xs">Latest run {latest.run_id}: {latest.status}, {latest.rows} rows</p>}
      {latest?.message && latest.run_id !== source.first_run?.run_id && <p role="status" className="mt-1 text-xs text-destructive">Latest run: {latest.message}</p>}
    </li>;
  })}</ul>;
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
function ControlDocument({ store, applied, document, onApplied }: { store: string; applied: number | null; document: string; onApplied: () => void }) {
  const [text, setText] = useState("");
  const [draft, setDraft] = useState<{ nonce: string; expected: number } | null>(null);
  const [result, setResult] = useState("");
  const [busy, setBusy] = useState<"edit" | "apply" | null>(null);
  const [identity, setIdentity] = useState("");
  const [connector, setConnector] = useState("http");
  const [endpoint, setEndpoint] = useState("");
  const [destination, setDestination] = useState("items");
  const [schedule, setSchedule] = useState("");
  const [configuration, setConfiguration] = useState("{}");
  useEffect(() => { setText(document); setDraft(null); }, [document, applied]);

  async function run(action: "edit" | "apply", source?: object) {
    if (applied === null || !Number.isSafeInteger(applied) || !store) {
      setResult("Select a store with an applied version.");
      return;
    }
    if (action === "apply" && (!draft || draft.expected !== applied)) {
      setResult("Save a draft before applying.");
      return;
    }
    setBusy(action);
    try {
      const body = action === "edit" ? { store, expected: applied, ...(source ? { source } : { document: text }) } : { store, expected: draft!.expected, nonce: draft!.nonce };
      const answer = await post<{ nonce?: string; document?: string }>(`/admin/api/${action}`, body);
      setResult(action === "edit" ? "Configuration validated. Review the draft, then apply." : "Version applied. Waiting for its first run; refresh outcomes to check progress.");
      if (action === "edit") {
        setDraft(answer.nonce ? { nonce: answer.nonce, expected: applied } : null);
        if (answer.document !== undefined) setText(answer.document);
      }
      else {
        setDraft(null);
        onApplied();
      }
    } catch (error) {
      setResult(error instanceof Error ? error.message : "Request failed");
    } finally {
      setBusy(null);
    }
  }

  function saveSource() {
    try {
      const config = JSON.parse(configuration) as unknown;
      if (config === null || typeof config !== "object" || Array.isArray(config)) throw new Error("source.config: Enter a JSON object.");
      void run("edit", { id: identity.trim(), tables: destination.split(",").map((table) => table.trim()).filter(Boolean),
        ...(schedule.trim() ? { schedule: schedule.trim() } : {}), source: { name: connector.trim(), config: { ...config, ...(connector.trim() === "http" ? { endpoint: endpoint.trim() } : {}) } } });
    } catch {
      setResult("source.config: Enter a valid JSON object for connector options.");
    }
  }

  return (
    <section aria-labelledby="control-title" className="flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <h2 id="control-title" className="flex items-center gap-1.5 text-[11px] font-medium tracking-wider text-muted-foreground uppercase">
          <FileCode2Icon className="size-3" /> Add ingest source
        </h2>
        <p id="version" className="text-xs text-muted-foreground">
          {applied === null ? "No applied version" : <>Applied version <span className="font-mono text-foreground">{applied}</span></>}
          {draft && <Badge variant="warning" className="ml-2">Validated draft saved</Badge>}
        </p>
      </div>
      <form onSubmit={(event) => { event.preventDefault(); saveSource(); }} onChange={() => setDraft(null)} className="grid gap-3 rounded-xl border border-border p-4 sm:grid-cols-2">
        <fieldset disabled={busy !== null} className="contents">
        <SourceField id="source-id" label="Source identity" value={identity} onChange={setIdentity} placeholder="inbox" />
        <SourceField id="source-connector" label="Connector name or component artifact" value={connector} onChange={setConnector} placeholder="http or connectors/source.wasm" />
        {connector.trim() === "http" && <SourceField id="source-endpoint" label="HTTP endpoint" value={endpoint} onChange={setEndpoint} placeholder="https://api.example/items" />}
        <SourceField id="source-destination" label="Output tables (comma separated)" value={destination} onChange={setDestination} placeholder="items" />
        <SourceField id="source-schedule" label="Schedule (empty for manual trigger)" value={schedule} onChange={setSchedule} placeholder="every 1h" />
        <label htmlFor="source-config" className="flex flex-col gap-1 text-xs sm:col-span-2">Connector options (JSON)<Textarea id="source-config" value={configuration} onChange={(event) => setConfiguration(event.target.value)} className="min-h-20 font-mono text-xs" /></label>
        <p className="text-xs text-muted-foreground sm:col-span-2">Output tables land under the source identity prefix. Credentials use secret references in connector options.</p>
        <Button type="submit" variant="outline" disabled={busy !== null || !store || applied === null}>{busy === "edit" ? <LoaderIcon className="animate-spin" /> : <SaveIcon />} Validate and save source draft</Button>
        </fieldset>
      </form>
      <details>
        <summary className="cursor-pointer text-sm">Raw TOML document</summary>
        <label htmlFor="document" className="sr-only">TOML document</label>
        <Textarea id="document" value={text} disabled={busy !== null} onChange={(event) => { setText(event.target.value); setDraft(null); }} spellCheck={false}
          className="mt-3 min-h-48 font-mono text-xs" />
      </details>
      <div className="flex items-center gap-2">
        <Button id="edit" type="button" variant="outline" disabled={busy !== null} onClick={() => void run("edit")}>
          {busy === "edit" ? <LoaderIcon className="animate-spin" /> : <SaveIcon />} Save draft
        </Button>
        <Button id="apply" type="button" disabled={busy !== null || !draft || draft.expected !== applied} onClick={() => void run("apply")}>
          {busy === "apply" ? <LoaderIcon className="animate-spin" /> : <CheckIcon />} Apply draft
        </Button>
        <p id="result" role="status" className="text-xs text-muted-foreground">{result}</p>
      </div>
    </section>
  );
}

function SourceField({ id, label, value, onChange, placeholder }: { id: string; label: string; value: string; onChange: (value: string) => void; placeholder: string }) {
  return <label htmlFor={id} className="flex flex-col gap-1 text-xs">{label}<input id={id} value={value} onChange={(event) => onChange(event.target.value)} placeholder={placeholder}
    className="rounded-lg border border-border bg-background px-3 py-2 text-sm" /></label>;
}
