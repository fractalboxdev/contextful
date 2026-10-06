export type ToolKind = "read" | "write";

type ToolBase = {
  name: string;
  pack: string;
  access?: "governed" | "direct-file";
};
export type Tool = ToolBase & (
  | { kind: "read"; leg?: "store"; table: string }
  | { kind: "read"; leg: "web"; table?: never }
  | { kind: "write"; leg?: "store" | "web"; table?: string }
);

export type ToolCall = { tool: string; arguments: Record<string, unknown>; publicationBound?: string };
export type Source = { id: string; label: string; url?: string };
export type ToolResult = { rows: unknown[]; sources: Source[] };
export type TurnRequest = { question: string; packs: string[]; store?: string; vantage?: string };
export type DataTable = { name: string; kind: "data" | "memory" };
export type Pack = { name: string; face: "organization" | "store"; tools: Array<{ name: string; kind: ToolKind }> };

export class ConsoleError extends Error {
  readonly code: string;
  readonly status: number;

  constructor(code: string, message = code, status = 400) {
    super(message);
    this.name = "ConsoleError";
    this.code = code;
    this.status = status;
  }
}

export function validatePack(pack: Pack): void {
  if (pack.face === "organization") {
    const write = pack.tools.find((tool) => tool.kind === "write");
    if (write) throw new ConsoleError("ConsoleWriteToolOnOrgFace", `${pack.name}: ${write.name}`);
  }
}

export function parseVantage(value: string): string {
  const year = Number(value.slice(0, 4));
  const month = Number(value.slice(5, 7));
  const day = Number(value.slice(8, 10));
  const validDay = Number.isInteger(year) && Number.isInteger(month) && Number.isInteger(day)
    && new Date(Date.UTC(year, month - 1, day)).toISOString().slice(0, 10) === value.slice(0, 10);
  if (/^\d{4}-\d{2}-\d{2}$/.test(value)) {
    const date = new Date(`${value}T00:00:00Z`);
    if (validDay && !Number.isNaN(date.getTime()) && date.toISOString().slice(0, 10) === value) return value;
  } else if (/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/.test(value)) {
    const date = new Date(value);
    if (validDay && !Number.isNaN(date.getTime())) return value;
  }
  throw new ConsoleError("ConsoleVantageUnparseable", value, 400);
}

export function parseWebBound(value: string): string {
  try { return parseVantage(value); }
  catch { throw new ConsoleError("ConsoleWebBoundUnparseable", value); }
}

export function sampleArrivals(arrivals: Array<{ table: string; labels: string[] }>) {
  return arrivals.map((arrival) => ({ table: arrival.table, labels: arrival.labels.slice(0, 3) }));
}

export async function resolveReaderCredential(options: {
  mint: () => Promise<string>;
  shared?: string;
}): Promise<string> {
  try {
    const credential = await options.mint();
    if (credential) return credential;
  } catch (error) {
    if (!(error instanceof ConsoleError && error.code === "ConsoleTokenExchangeRefused" && error.status === 403)) throw error;
  }
  if (options.shared) return options.shared;
  throw new ConsoleError("ConsoleTokenExchangeRefused", "ConsoleTokenExchangeRefused", 403);
}

export class OverlayCache {
  private entries = new Map<string, { until: number; value: string | null }>();
  private readonly read: (store: string) => Promise<string | null>;
  private readonly clock: () => number;

  constructor(
    read: (store: string) => Promise<string | null>,
    clock: () => number = Date.now,
  ) {
    this.read = read;
    this.clock = clock;
  }

  async get(store: string): Promise<string> {
    const cached = this.entries.get(store);
    if (cached && this.clock() < cached.until) return cached.value ?? "";
    const value = (await this.read(store))?.slice(0, 8000) ?? null;
    this.entries.set(store, { until: this.clock() + 300_000, value });
    return value ?? "";
  }
}

export class StreamingRedactor {
  private pending = "";
  private readonly denylist: string[];

  constructor(denylist: string[]) {
    if (denylist.some((entry) => entry.length > 128)) throw new RangeError("denylist entry exceeds 128 chars");
    this.denylist = [...new Set(denylist.filter(Boolean))].sort((a, b) => b.length - a.length);
  }

  push(chunk: string): string {
    this.pending += chunk;
    return this.release(Math.max(0, this.pending.length - 128));
  }

  finish(): string {
    return this.release(this.pending.length);
  }

  private release(until: number): string {
    let output = "";
    let index = 0;
    while (index < until) {
      const denied = this.denylist.find((entry) => this.pending.startsWith(entry, index));
      if (denied) {
        output += "[redacted]";
        index += denied.length;
      } else {
        output += this.pending[index];
        index++;
      }
    }
    this.pending = this.pending.slice(index);
    return output;
  }
}

type PlannerInput = {
  question: string;
  vantage?: string;
  tools: Tool[];
  tables: DataTable[];
  previous: ToolResult[];
};
type SynthesisInput = {
  question: string;
  vantage?: string;
  results: ToolResult[];
  overlay: string;
  sources: Source[];
};

export type TurnOptions = {
  tools: Tool[];
  packs?: Pack[];
  tables?: DataTable[];
  planner: (input: PlannerInput) => Promise<ToolCall[]>;
  transport: { call: (call: ToolCall & { vantage?: string; maxRows: number; signal: AbortSignal }) => Promise<ToolResult> };
  synthesize: (input: SynthesisInput) => AsyncIterable<string>;
  overlay?: (store: string) => Promise<string | null>;
  denylist?: string[];
  clock?: () => number;
  timeoutMs?: number;
};

export function createTurn(options: TurnOptions) {
  options.packs?.forEach(validatePack);
  const overlay = new OverlayCache(options.overlay ?? (async () => null), options.clock);

  return {
    async ask(request: TurnRequest): Promise<{ text: string; sources: Source[] }> {
      const vantage = request.vantage === undefined ? undefined : parseVantage(request.vantage);
      const admitted = options.tools.filter((tool) => request.packs.includes(tool.pack));
      const results: ToolResult[] = [];
      const rowsByTable = new Map<string, number>();
      const deadline = Date.now() + Math.min(options.timeoutMs ?? 10_000, 10_000);
      for (let round = 0; round < 2; round++) {
        const memoryTable = (name: string | undefined) => options.tables?.some((table) => table.kind === "memory" && table.name === name);
        const calls = await options.planner({
          question: request.question,
          vantage,
          tools: admitted.filter((tool) => tool.kind === "read" && !memoryTable(tool.table)),
          tables: (options.tables ?? []).filter((table) => table.kind === "data"),
          previous: results,
        });
        for (const call of calls) {
          const tool = options.tools.find((candidate) => candidate.name === call.tool);
          if (!tool || !request.packs.includes(tool.pack)) throw new ConsoleError("ConsoleToolNotAdmitted", call.tool);
          if (tool.kind !== "read") throw new ConsoleError("ConsoleMutatingToolRequested", call.tool);
          if (memoryTable(tool.table)) throw new ConsoleError("ConsolePlannerReachedMemory", call.tool);
          if (tool.access === "direct-file") throw new ConsoleError("ConsoleFileAccessDirect", call.tool);
          if (tool.leg === "web" && call.publicationBound !== undefined) parseWebBound(call.publicationBound);
          const table = tool.leg === "web" ? undefined : tool.table;
          if (tool.leg !== "web" && (!table || !options.tables?.some((entry) => entry.kind === "data" && entry.name === table))) {
            throw new RangeError(`tool ${call.tool} has no registered data table`);
          }
          const consumed = table === undefined ? 0 : rowsByTable.get(table) ?? 0;
          const remaining = 5000 - consumed;
          if (remaining <= 0) throw new RangeError("tool exceeded 5000 rows per data table");
          const controller = new AbortController();
          const milliseconds = Math.max(0, deadline - Date.now());
          let timeout: ReturnType<typeof setTimeout> | undefined;
          try {
            const result = await Promise.race([
              options.transport.call({ ...call, vantage, maxRows: remaining, signal: controller.signal }),
              new Promise<never>((_, reject) => { timeout = setTimeout(() => {
                controller.abort();
                reject(new Error("code path exceeded 10 s"));
              }, milliseconds); }),
            ]);
            if (result.rows.length > remaining) throw new RangeError("tool returned more than 5000 rows per data table");
            if (table !== undefined) rowsByTable.set(table, consumed + result.rows.length);
            results.push(result);
          } finally {
            if (timeout) clearTimeout(timeout);
          }
        }
        if (results.some((result) => result.rows.length > 0)) break;
      }
      if (!results.some((result) => result.rows.length > 0)) throw new ConsoleError("ConsoleUngroundedAnswer", "The store holds no matching information.");

      const sources = [...new Map(results.flatMap((result) => result.sources).map((source) => [source.id, source])).values()].slice(0, 8);
      if (sources.length === 0) throw new ConsoleError("ConsoleUngroundedAnswer", "The store holds no sourced information.");
      const redactor = new StreamingRedactor(options.denylist ?? []);
      let answer = "";
      for await (const chunk of options.synthesize({ question: request.question, vantage, results, overlay: await overlay.get(request.store ?? "default"), sources })) {
        answer += redactor.push(chunk);
      }
      answer += redactor.finish();
      const citations = new Map(sources.map((source, index) => [source.id, `source-${index + 1}`]));
      answer = answer.replace(/\[([^\]\n]+)\]/g, (match, id: string) => id === "redacted" ? match : citations.has(id) ? `[${citations.get(id)}]` : "");
      const safeSources = sources.map((source, index) => {
        const labelRedactor = new StreamingRedactor(options.denylist ?? []);
        const label = labelRedactor.push(source.label) + labelRedactor.finish();
        const safeLabel = label.replace(/[\r\n]/g, " ");
        const url = source.url && /^https?:\/\//.test(source.url)
          && !(options.denylist ?? []).some((entry) => entry && source.url?.includes(entry)) ? source.url : undefined;
        return { id: `source-${index + 1}`, label: safeLabel, url };
      });
      const sourceLines = safeSources.map((source) => `- ${source.label}${source.url ? ` (${source.url})` : ""}`);
      return { text: `${answer.trim()}\n\nSources\n${sourceLines.join("\n")}`, sources: safeSources };
    },
  };
}
