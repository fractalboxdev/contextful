import { useState, type ReactNode } from "react";
import { ClockIcon, DatabaseIcon, MenuIcon, MessageSquareIcon, Settings2Icon, XIcon } from "lucide-react";
import { cn } from "./ui.tsx";

export type Store = { id: string; label: string };
export type Page = "query" | "admin";

const pages: Array<{ page: Page; href: string; label: string; icon: typeof MessageSquareIcon }> = [
  { page: "query", href: "/query", label: "Query", icon: MessageSquareIcon },
  { page: "admin", href: "/admin", label: "Admin", icon: Settings2Icon },
];

/** The console frame: a sidebar carrying the brand, page links and page-specific
 *  content, and a main column under the shared header bar. Below the `md` breakpoint
 *  the sidebar opens as a drawer from a button beside the header. */
export function Shell({ page, sidebar, header, children }: { page: Page; sidebar?: ReactNode; header: ReactNode; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="flex h-dvh">
      {open && <div className="fixed inset-0 z-30 bg-background/60 md:hidden" onClick={() => setOpen(false)} aria-hidden />}
      <aside id="console-sidebar" className={cn("w-64 shrink-0 flex-col border-r border-border bg-card md:static md:flex md:bg-card/40",
        open ? "fixed inset-y-0 left-0 z-40 flex" : "hidden")}>
        <div className="flex items-center gap-2 px-3 pt-3 pb-2">
          <span className="text-sm font-semibold text-foreground">Contextful</span>
          <span className="rounded-full border border-border px-2 py-0.5 text-[11px] text-muted-foreground">Console</span>
          <button type="button" onClick={() => setOpen(false)} aria-label="Close chats"
            className="ml-auto text-muted-foreground hover:text-foreground md:hidden">
            <XIcon className="size-4" />
          </button>
        </div>
        <nav aria-label="Console" className="space-y-0.5 px-2 pb-2">
          {pages.map((item) => (
            <a key={item.page} href={item.href} aria-current={item.page === page ? "page" : undefined}
              className={cn("flex items-center gap-2 rounded-lg px-2 py-1.5 text-sm",
                item.page === page ? "bg-accent text-foreground" : "text-muted-foreground hover:bg-accent/50 hover:text-foreground")}>
              <item.icon className="size-3.5" /> {item.label}
            </a>
          ))}
        </nav>
        <div className="min-h-0 flex-1 overflow-y-auto border-t border-border px-2 py-2 scrollbar-thin"
          onClick={(event) => {
            const button = (event.target as Element).closest("button");
            if (button && button.getAttribute("aria-label") !== "Delete chat") setOpen(false);
          }}>
          {sidebar}
        </div>
      </aside>
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex items-stretch">
          {sidebar && (
            <button type="button" onClick={() => setOpen(true)} aria-controls="console-sidebar" aria-expanded={open} aria-label="Open chats"
              className="flex items-center border-b border-border pl-4 text-muted-foreground hover:text-foreground md:hidden">
              <MenuIcon className="size-4" />
            </button>
          )}
          <div className="min-w-0 flex-1">{header}</div>
        </div>
        <div className="min-h-0 flex-1">{children}</div>
      </div>
    </div>
  );
}

/** The shared header bar: leading slot, title, right-aligned controls. */
export function TopNav({ page, title, children }: { page: Page; title: ReactNode; children?: ReactNode }) {
  return (
    <header className="flex flex-wrap items-center gap-3 border-b border-border px-4 py-2.5">
      <nav aria-label="Pages" className="flex items-center gap-1 md:hidden">
        {pages.map((item) => (
          <a key={item.page} href={item.href} aria-current={item.page === page ? "page" : undefined}
            className={cn("rounded-lg px-2 py-1 text-xs", item.page === page ? "bg-accent text-foreground" : "text-muted-foreground")}>
            {item.label}
          </a>
        ))}
      </nav>
      <span className="text-sm text-muted-foreground">{title}</span>
      <div className="ml-auto flex items-center gap-3">{children}</div>
    </header>
  );
}

/** The context-store filter. */
export function StoreSelect({ id, stores, value, onChange }: { id: string; stores: Store[]; value: string; onChange: (id: string) => void }) {
  return (
    <label htmlFor={id} className="flex items-center gap-2 text-xs text-muted-foreground">
      <DatabaseIcon className="size-3.5" />
      <span className="hidden sm:inline">Store</span>
      <select id={id} required value={value} onChange={(event) => onChange(event.target.value)}
        className="rounded-lg border border-border bg-background px-2.5 py-1 text-sm text-foreground">
        {stores.map((store) => <option key={store.id} value={store.id}>{store.label}</option>)}
      </select>
    </label>
  );
}

/** The time-travel vantage: an empty value reads the latest data. */
export function AsOfSelect({ id, value, onChange }: { id: string; value: string; onChange: (value: string) => void }) {
  return (
    <label htmlFor={id} className="flex items-center gap-1.5 text-xs text-muted-foreground" title="Read the store as it stood on a past day">
      <ClockIcon className="size-3.5" />
      <span className="hidden sm:inline">As of</span>
      <input id={id} type="date" value={value} onChange={(event) => onChange(event.target.value)}
        className={cn("rounded-lg border bg-background px-2.5 py-1 text-sm text-foreground [color-scheme:dark]",
          value ? "border-primary/60" : "border-border")} />
      {value && (
        <button type="button" onClick={() => onChange("")} className="text-xs text-muted-foreground hover:text-foreground">Latest</button>
      )}
    </label>
  );
}

export async function post<T>(path: string, payload: unknown): Promise<T> {
  const response = await fetch(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(payload) });
  const value = await response.json() as T & { error?: { identifier?: string } };
  if (!response.ok) throw new Error(value.error?.identifier ?? `HTTP ${response.status}`);
  return value;
}
