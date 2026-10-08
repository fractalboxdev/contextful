import { useEffect, useState } from "react";
import { ArrowRightIcon, CalendarClockIcon, CheckIcon, FileCode2Icon, LoaderIcon, SaveIcon, WorkflowIcon } from "lucide-react";
import { post, Shell, StoreSelect, TopNav, type Store } from "./shell.tsx";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle, Textarea } from "./ui.tsx";

type Run = { status?: string; run_id?: string };
type Pipeline = { id: string; schedule?: string; steps?: string[] };
type Workflows = { applied?: number | null; pipelines?: Pipeline[]; runs?: Record<string, Run> };
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
            <p className="text-sm text-muted-foreground">Pipelines, schedules, steps and run outcomes come from the store.</p>
          </div>
          <section aria-labelledby="canvas-title" className="flex flex-col gap-3">
            <h2 id="canvas-title" className="flex items-center gap-1.5 text-[11px] font-medium tracking-wider text-muted-foreground uppercase">
              <WorkflowIcon className="size-3" /> Workflow canvas
            </h2>
            <div id="canvas" className="grid gap-3 sm:grid-cols-2">
              {workflows === null ? <span className="shimmer text-xs font-medium">Reading workflow state…</span> :
                "error" in workflows ? <p className="text-sm text-destructive">{workflows.error}</p> :
                (workflows.value.pipelines ?? []).length === 0 ? <p className="text-sm text-muted-foreground">No workflows are published.</p> :
                workflows.value.pipelines!.map((pipeline) => (
                  <PipelineCard key={pipeline.id} pipeline={pipeline} run={workflows.value.runs?.[pipeline.id]} />
                ))}
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

function PipelineCard({ pipeline, run }: { pipeline: Pipeline; run?: Run }) {
  return (
    <Card className="node gap-3">
      <CardHeader>
        <CardTitle className="font-mono">{pipeline.id}</CardTitle>
        {pipeline.schedule && (
          <Badge variant="secondary" className="font-mono"><CalendarClockIcon /> {pipeline.schedule}</Badge>
        )}
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        {(pipeline.steps?.length ?? 0) > 0 && (
          <ol className="flex flex-wrap items-center gap-1.5">
            {pipeline.steps!.map((step, index) => (
              <li key={`${step}-${index}`} className="flex items-center gap-1.5">
                {index > 0 && <ArrowRightIcon className="size-3 text-muted-foreground" aria-hidden />}
                <span className="rounded-md border border-border bg-muted/40 px-2 py-0.5 font-mono text-xs">{step}</span>
              </li>
            ))}
          </ol>
        )}
        <div className="flex flex-wrap items-center gap-1.5 text-xs text-muted-foreground">
          {run ? (
            <>
              <Badge variant={runVariant(run.status)}>{run.status ?? "unknown"}</Badge>
              {run.run_id && <span className="font-mono">{run.run_id}</span>}
            </>
          ) : "No runs recorded"}
        </div>
      </CardContent>
    </Card>
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
