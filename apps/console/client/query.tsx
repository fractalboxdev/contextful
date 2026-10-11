import { useEffect, useRef, useState, type ReactNode } from "react";
import {
  ArrowUpIcon, BarChart3Icon, ClockIcon, DatabaseIcon, FolderIcon, LoaderIcon, MessageSquareIcon, PlusIcon, SparklesIcon, Trash2Icon, XIcon,
} from "lucide-react";
import { ColumnarResult, WidgetView, type Rows, type Widget } from "./result.tsx";
import { AsOfSelect, post, Shell, StoreSelect, TopNav, type Store } from "./shell.tsx";
import { restoreSaved, saveSaved } from "./transcripts.ts";
import { Bubble, Button, cn, Message, MessageAvatar, MessageContent, MessageHeader, Textarea } from "./ui.tsx";

type Source = { id?: string; label?: string; title?: string; url?: string };
type Turn = { id: number; role: "user" | "assistant"; text: string; sources?: Source[]; widgets?: Widget[]; status: "pending" | "done" | "error" };
type Session = { id: string; store: string; title: string; turns: Turn[]; updatedAt: number };
type Browse = {
  chips: Array<{ table: string; label: string; description?: string }>;
  insights: Array<{ table: string; label: string; rows: number }>;
  files: Array<{ table: string; path: string; label: string }>;
};
type Brief = { windowDays: number; subjects: Array<{ subject: string; articles: Array<{ id: string; label: string }> }> };

// Tab-scoped and operator-scoped: transcripts carry answer rows, and a shared browser must
// not hand one operator's answers to the next.
const SESSIONS_KEY = "contextful-console-sessions";

function newSession(store: string): Session {
  return { id: `s_${Date.now().toString(36)}_${Math.floor(Math.random() * 1e9).toString(36)}`, store, title: "New chat", turns: [], updatedAt: Date.now() };
}

function readSessions(scope: string, stores: Store[]): Session[] {
  try {
    return restoreSaved<Session>(sessionStorage.getItem(SESSIONS_KEY), scope, stores);
  } catch {
    return [];
  }
}

export function QueryApp() {
  const [stores, setStores] = useState<Store[]>([]);
  const [sessions, setSessions] = useState<Session[]>([]);
  const [activeId, setActiveId] = useState("");
  const [asOf, setAsOf] = useState("");
  const [scope, setScope] = useState("");
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    fetch("/query/api/stores").then(async (response) => ({
      scope: response.headers.get("x-console-transcript-scope") ?? "",
      list: await response.json() as Store[],
    })).catch(() => ({ scope: "", list: [] })).then(({ scope: verified, list }) => {
      const known = Array.isArray(list) ? list : [];
      setStores(known);
      setScope(verified);
      let initial = readSessions(verified, known);
      if (initial.length === 0) initial = [newSession(known[0]?.id ?? "")];
      setSessions(initial);
      setActiveId(initial[0].id);
      setLoaded(true);
    });
  }, []);

  useEffect(() => {
    if (!loaded || !scope) return;
    try { sessionStorage.setItem(SESSIONS_KEY, saveSaved(scope, sessions)); } catch { /* storage unavailable */ }
  }, [sessions, scope, loaded]);

  const active = sessions.find((session) => session.id === activeId);
  const store = active?.store ?? stores[0]?.id ?? "";

  function startNew() {
    const session = newSession(store);
    setSessions((previous) => [session, ...previous]);
    setActiveId(session.id);
  }

  function remove(id: string) {
    setSessions((previous) => {
      const next = previous.filter((session) => session.id !== id);
      if (next.length === 0) {
        const session = newSession(store);
        setActiveId(session.id);
        return [session];
      }
      if (id === activeId) setActiveId(next[0].id);
      return next;
    });
  }

  function changeStore(next: string) {
    setAsOf("");
    if (active && active.turns.length > 0) {
      const session = newSession(next);
      setSessions((previous) => [session, ...previous]);
      setActiveId(session.id);
    } else {
      setSessions((previous) => previous.map((session) => session.id === activeId ? { ...session, store: next } : session));
    }
  }

  function onTurns(update: (turns: Turn[]) => Turn[]) {
    setSessions((previous) => previous.map((session) => {
      if (session.id !== activeId) return session;
      const turns = update(session.turns);
      const first = turns.find((turn) => turn.role === "user");
      const title = session.title === "New chat" && first ? (first.text.length > 40 ? `${first.text.slice(0, 40)}…` : first.text) : session.title;
      return { ...session, turns, title, updatedAt: Date.now() };
    }));
  }

  const sidebar = (
    <>
      <div className="flex items-center px-1 pb-1">
        <span className="text-[11px] font-medium tracking-wider text-muted-foreground uppercase">Chats</span>
        <button type="button" onClick={startNew}
          className="ml-auto flex items-center gap-1.5 rounded-lg border border-border px-2.5 py-1 text-xs hover:bg-accent">
          <PlusIcon className="size-3.5" /> New chat
        </button>
      </div>
      <div className="space-y-0.5">
        {sessions.map((session) => (
          <div key={session.id} className={cn("group flex items-center gap-1 rounded-lg px-2 py-1.5 text-sm",
            session.id === activeId ? "bg-accent" : "hover:bg-accent/50")}>
            <button type="button" onClick={() => setActiveId(session.id)} className="flex min-w-0 flex-1 items-center gap-2 text-left">
              <MessageSquareIcon className="size-3.5 shrink-0 text-muted-foreground" />
              <span className="truncate">{session.title}</span>
            </button>
            <button type="button" onClick={() => remove(session.id)} title="Delete chat" aria-label="Delete chat"
              className="shrink-0 text-muted-foreground transition hover:text-destructive focus-visible:opacity-100 md:opacity-0 md:group-hover:opacity-100">
              <Trash2Icon className="size-3.5" />
            </button>
          </div>
        ))}
      </div>
    </>
  );

  return (
    <Shell page="query" sidebar={sidebar} header={
      <TopNav page="query" title="Ask the store">
        <AsOfSelect id="as-of" value={asOf} onChange={setAsOf} />
        <StoreSelect id="store" stores={stores} value={store} onChange={changeStore} />
      </TopNav>
    }>
      <Conversation key={active?.id ?? "fresh"} store={store} storeLabel={stores.find((entry) => entry.id === store)?.label}
        asOf={asOf} turns={active?.turns ?? []} onTurns={onTurns} />
    </Shell>
  );
}

function Conversation({ store, storeLabel, asOf, turns, onTurns }: {
  store: string; storeLabel?: string; asOf: string; turns: Turn[]; onTurns: (update: (turns: Turn[]) => Turn[]) => void;
}) {
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [brief, setBrief] = useState<Brief | null>(null);
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const endRef = useRef<HTMLDivElement | null>(null);
  const fresh = turns.length === 0;

  useEffect(() => {
    setBrief(null);
    if (!store || !fresh || asOf) return;
    let ignore = false;
    fetch(`/query/api/brief?store=${encodeURIComponent(store)}`)
      .then((response) => response.ok && response.status !== 204 ? response.json() as Promise<Brief> : null)
      .then((card) => { if (!ignore) setBrief(card); })
      .catch(() => { if (!ignore) setBrief(null); });
    return () => { ignore = true; };
  }, [store, fresh, asOf]);

  useEffect(() => { endRef.current?.scrollIntoView({ block: "end" }); }, [turns.length]);

  function prefill(text: string) {
    setInput(text);
    setMenuOpen(false);
    inputRef.current?.focus();
  }

  async function send(question: string) {
    const text = question.trim();
    if (!text || busy || !store) return;
    setMenuOpen(false);
    const base = turns.reduce((max, turn) => Math.max(max, turn.id), 0);
    const answerId = base + 2;
    onTurns((previous) => [...previous, { id: base + 1, role: "user", text, status: "done" },
      { id: answerId, role: "assistant", text: "", status: "pending" }]);
    setInput("");
    setBusy(true);
    try {
      const answer = await post<{ answer: string; sources?: Source[]; widgets?: Widget[] }>("/query/api/ask", { store, question: text });
      onTurns((previous) => previous.map((turn) => turn.id === answerId ?
        { ...turn, text: answer.answer, sources: answer.sources ?? [], widgets: answer.widgets ?? [], status: "done" } : turn));
    } catch (error) {
      onTurns((previous) => previous.map((turn) => turn.id === answerId ?
        { ...turn, text: error instanceof Error ? error.message : "Request failed", status: "error" } : turn));
    } finally {
      setBusy(false);
    }
  }

  const composer = (withMenu: boolean) => (
    <form id="composer" onSubmit={(event) => { event.preventDefault(); void send(input); }} className="relative">
      {withMenu && (
        <button type="button" onClick={() => setMenuOpen((open) => !open)} aria-label="Browse the store" aria-expanded={menuOpen}
          className="absolute bottom-2 left-2 flex size-7 items-center justify-center rounded-full border border-border text-muted-foreground hover:bg-accent hover:text-foreground">
          {menuOpen ? <XIcon className="size-4" /> : <PlusIcon className="size-4" />}
        </button>
      )}
      <label htmlFor="question" className="sr-only">Question</label>
      <Textarea id="question" ref={inputRef} required rows={1} value={input} onChange={(event) => setInput(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault();
            void send(input);
          }
        }}
        placeholder={`Ask about ${storeLabel ?? "this store"}${asOf ? ` (as of ${asOf})` : ""}…`}
        className={cn("max-h-40 min-h-12 resize-none rounded-2xl pr-12", withMenu && "pl-11")} />
      <Button type="submit" size="icon-sm" disabled={busy || !input.trim()} className="absolute right-2 bottom-2 rounded-full">
        {busy ? <LoaderIcon className="animate-spin" /> : <ArrowUpIcon />}
        <span className="sr-only">Ask question</span>
      </Button>
    </form>
  );

  const browse = <BrowsePanel store={store} asOf={asOf} busy={busy} onPick={prefill} />;

  return (
    <div className="flex h-full flex-col bg-background">
      <div className="min-h-0 flex-1 overflow-y-auto scrollbar-thin">
        <div id="transcript" role="log" aria-live="polite" className="mx-auto flex w-full max-w-3xl flex-col gap-6 px-4 py-6">
          <TimeBasis asOf={asOf} />
          {fresh ? (
            <div className="flex flex-col gap-4 py-6">
              <div className="flex flex-col items-center gap-3 text-center">
                <div className="flex size-11 items-center justify-center rounded-2xl bg-muted"><SparklesIcon className="size-5" /></div>
                <h1 className="text-xl font-semibold">Ask the store</h1>
                <p className="max-w-md text-sm text-muted-foreground">
                  Ask in plain English about <span className="font-medium text-foreground">{storeLabel ?? "this store"}</span>.
                  Answers cite the rows your access permits.
                </p>
              </div>
              <BriefCard brief={brief && brief.subjects.length > 0 ? brief : null} onPick={prefill} />
              {composer(false)}
              {browse}
              <div id="widgets" />
            </div>
          ) : turns.map((turn, index) => (
            <TurnView key={turn.id} turn={turn} latest={index === turns.length - 1} />
          ))}
          <div ref={endRef} />
        </div>
      </div>
      {!fresh && (
        <div className="border-t border-border bg-background/80 px-4 py-3 backdrop-blur">
          <div className="relative mx-auto w-full max-w-3xl">
            {menuOpen && (
              <>
                <div className="fixed inset-0 z-10" onClick={() => setMenuOpen(false)} aria-hidden />
                <div className="absolute bottom-full left-0 z-20 mb-2 max-h-[60vh] w-full overflow-y-auto rounded-2xl border border-border bg-popover p-3 shadow-xl scrollbar-thin">
                  {browse}
                </div>
              </>
            )}
            {composer(true)}
          </div>
        </div>
      )}
    </div>
  );
}

function TimeBasis({ asOf }: { asOf: string }) {
  return (
    <div className="flex justify-center">
      <span className={cn("inline-flex items-center gap-1.5 rounded-full border px-3 py-1 text-[11px] text-muted-foreground",
        asOf ? "border-primary/40 bg-primary/5" : "border-border bg-muted/40")}>
        <ClockIcon className={cn("size-3", asOf && "text-primary")} />
        {asOf ? <span>Browsing as of <span className="font-medium text-foreground">{asOf}</span></span> : <span>Reading the latest data</span>}
      </span>
    </div>
  );
}

function BriefCard({ brief, onPick }: { brief: Brief | null; onPick: (prompt: string) => void }) {
  if (!brief) return <section id="brief" hidden />;
  return (
    <section id="brief" className="rounded-xl border border-primary/30 bg-primary/5 px-4 py-3 text-left text-sm animate-in fade-in slide-in-from-top-1">
      <h2 className="flex items-center gap-1.5 text-xs font-medium text-foreground">
        <ClockIcon className="size-3.5 text-primary" /> Since last visit
      </h2>
      <p className="mt-0.5 text-xs text-muted-foreground">
        {brief.subjects.length} subject{brief.subjects.length === 1 ? "" : "s"} you follow changed over the past {brief.windowDays} days
      </p>
      <ul id="brief-body" className="mt-2 space-y-1.5">
        {brief.subjects.map((subject) => (
          <li key={subject.subject}>
            <button type="button" onClick={() => onPick(`What changed for ${subject.subject}?`)}
              className="w-full rounded-lg border border-transparent px-2 py-1.5 text-left transition hover:border-primary/30 hover:bg-primary/10">
              <span className="flex items-baseline gap-2">
                <span className="font-medium text-foreground">{subject.subject}</span>
                <span className="text-xs text-muted-foreground">{subject.articles.length} new article{subject.articles.length === 1 ? "" : "s"}</span>
              </span>
              <span className="mt-0.5 block truncate text-xs text-muted-foreground/80">
                {subject.articles.map((article) => article.label).join(" · ")}
              </span>
            </button>
          </li>
        ))}
      </ul>
    </section>
  );
}

function BrowsePanel({ store, asOf, busy, onPick }: { store: string; asOf: string; busy: boolean; onPick: (prompt: string) => void }) {
  const [data, setData] = useState<Browse | null>(null);
  const [status, setStatus] = useState("");
  const [preview, setPreview] = useState<{ label: string; rows: Rows | null } | null>(null);
  const scope = { store, ...(asOf ? { asOf } : {}) };

  useEffect(() => {
    setData(null);
    setPreview(null);
    setStatus("");
    if (!store) return;
    let ignore = false;
    post<Browse>("/query/api/browse", scope)
      .then((value) => { if (!ignore) setData(value); })
      .catch((error: unknown) => { if (!ignore) setStatus(error instanceof Error ? error.message : String(error)); });
    return () => { ignore = true; };
  }, [store, asOf]);

  async function open(file: Browse["files"][number]) {
    setPreview({ label: file.label, rows: null });
    try {
      setPreview({ label: file.label, rows: await post<Rows>("/query/api/preview", { ...scope, path: file.path }) });
    } catch (error) {
      setPreview(null);
      setStatus(error instanceof Error ? error.message : String(error));
    }
  }

  return (
    <div className="flex flex-col gap-3">
      <p id="browse-status" role="status" className={cn("text-xs text-destructive", !status && "sr-only")}>{status}</p>
      <Group icon={DatabaseIcon} title="Explore">
        <div id="chips" className="flex flex-wrap gap-1.5">
          {data === null && !status ? <span className="shimmer text-xs font-medium">Reading the store…</span> : data?.chips.map((chip) => (
            <Button key={chip.table} type="button" variant="outline" size="sm" disabled={busy} title={chip.description}
              onClick={() => onPick(`Tell me about ${chip.label}`)} className="rounded-full text-muted-foreground">
              {chip.label}
            </Button>
          ))}
        </div>
      </Group>
      <Group icon={BarChart3Icon} title="Insights">
        <div id="insights" className="grid grid-cols-2 gap-2 sm:grid-cols-3">
          {data?.insights.map((insight) => (
            <div key={insight.table} className="rounded-xl border border-border bg-card/40 px-3 py-2">
              <p className="truncate text-xs text-muted-foreground">{insight.label}</p>
              <p className="font-mono text-lg font-semibold tabular-nums">{insight.rows.toLocaleString("en-US")}</p>
              <p className="text-[11px] text-muted-foreground">rows</p>
            </div>
          ))}
        </div>
      </Group>
      <Group icon={FolderIcon} title="Files">
        <div id="file-gallery" className="flex flex-wrap gap-1.5">
          {data?.files.length === 0 && <span className="text-xs text-muted-foreground">No files are listed for this store.</span>}
          {data?.files.map((file) => (
            <button key={file.path} type="button" title={file.path} onClick={() => void open(file)}
              className="inline-flex items-center gap-1 rounded-full border border-border px-3 py-1 font-mono text-xs text-muted-foreground transition-colors hover:bg-accent hover:text-foreground">
              <FolderIcon className="size-3 opacity-70" /> {file.label}
            </button>
          ))}
        </div>
        <div id="file-preview" className="mt-2">
          {preview && (
            <div className="rounded-xl border border-border p-3">
              <div className="mb-2 flex items-center justify-between">
                <span className="font-mono text-xs text-muted-foreground">{preview.label}</span>
                <button type="button" onClick={() => setPreview(null)} aria-label="Close preview" className="text-muted-foreground hover:text-foreground">
                  <XIcon className="size-4" />
                </button>
              </div>
              {preview.rows ? <ColumnarResult {...preview.rows} /> : <span className="shimmer text-xs font-medium">Reading the file…</span>}
            </div>
          )}
        </div>
      </Group>
    </div>
  );
}

function Group({ icon: Icon, title, children }: { icon: typeof FolderIcon; title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-1.5">
      <h2 className="flex items-center gap-1.5 text-[11px] font-medium tracking-wider text-muted-foreground uppercase">
        <Icon className="size-3" /> {title}
      </h2>
      {children}
    </section>
  );
}

function TurnView({ turn, latest }: { turn: Turn; latest: boolean }) {
  if (turn.role === "user") {
    return (
      <Message align="end">
        <MessageContent><Bubble align="end">{turn.text}</Bubble></MessageContent>
      </Message>
    );
  }
  return (
    <Message align="start">
      <MessageAvatar><SparklesIcon className="size-4" /></MessageAvatar>
      <MessageContent>
        <MessageHeader>Contextful</MessageHeader>
        <Bubble variant="ghost">
          {turn.status === "pending" ? <span className="shimmer font-medium">Reading the store…</span> :
            turn.status === "error" ? <span className="text-destructive">{turn.text}</span> :
            <div className="leading-relaxed whitespace-pre-wrap text-foreground">{turn.text}</div>}
        </Bubble>
        <div id={latest ? "widgets" : undefined} className="space-y-4">
          {(turn.widgets ?? []).map((widget, index) => <WidgetView key={index} widget={widget} />)}
        </div>
        {(turn.sources?.length ?? 0) > 0 && (
          <div className="animate-in fade-in slide-in-from-bottom-1">
            <p className="text-[11px] font-medium tracking-wider text-muted-foreground uppercase">Sources</p>
            <ol className="mt-1 space-y-1">
              {turn.sources!.map((source, index) => (
                <li key={source.id ?? index} className="flex items-baseline gap-1.5 text-xs text-muted-foreground">
                  <span className="shrink-0 tabular-nums opacity-60">{index + 1}.</span>
                  {source.url ? (
                    <a href={source.url} target="_blank" rel="noopener noreferrer" className="underline decoration-dotted underline-offset-2 hover:text-foreground">
                      {source.label ?? source.title ?? source.url}
                    </a>
                  ) : <span>{source.label ?? source.title ?? source.id}</span>}
                </li>
              ))}
            </ol>
          </div>
        )}
      </MessageContent>
    </Message>
  );
}
