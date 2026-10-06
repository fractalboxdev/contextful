type ToolResult = {
  structuredContent?: unknown;
  isError?: boolean;
};

type ReadTransport = {
  call(tool: string, arguments_: Record<string, unknown>): Promise<ToolResult>;
};

type TableEntry = { table: string; description?: string; zone_admitted?: boolean };
type FileEntry = { table: string; path: string; label: string };
type BrowseRequest = { asOf?: string };
const MAX_TABLES = 64;
const MAX_FILES = 128;
const MAX_PREVIEW_ROWS = 100;

export class BrowseError extends Error {
  readonly code: string;

  constructor(code: string, message = code) {
    super(message);
    this.name = "BrowseError";
    this.code = code;
  }
}

const abbreviations = new Set(["API", "CSV", "PDF", "SEC", "SQL", "URL"]);

export function humanizeLabel(identifier: string): string {
  const tail = identifier.split("/").at(-1) ?? identifier;
  return tail.replace(/\.[^.]+$/, "").replace(/([a-z])([A-Z])/g, "$1 $2")
    .split(/[-_\s]+/).filter(Boolean)
    .map((part) => {
      const upper = part.toUpperCase();
      return abbreviations.has(upper) || /^[Qq]\d+$/.test(part)
        ? upper : part[0].toUpperCase() + part.slice(1).toLowerCase();
    }).join(" ");
}

function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new BrowseError("ConsoleBrowseResponseMalformed");
  return value as Record<string, unknown>;
}

function bounds(request: BrowseRequest): Record<string, unknown> {
  return request.asOf ? { as_of: request.asOf } : {};
}

export function createBrowse(transport: ReadTransport) {
  async function read(tool: string, arguments_: Record<string, unknown>): Promise<Record<string, unknown>> {
    const result = await transport.call(tool, arguments_);
    if (result.isError) {
      const error = object(object(result.structuredContent).error);
      throw new BrowseError(String(error.identifier ?? "ConsoleBrowseReadRefused"));
    }
    return object(result.structuredContent);
  }

  async function listed(request: BrowseRequest): Promise<{ tables: TableEntry[]; files: FileEntry[] }> {
    const listing = await read("context.describe", bounds(request));
    if (!Array.isArray(listing.tables)) throw new BrowseError("ConsoleBrowseResponseMalformed");
    const tables = listing.tables.map((item) => object(item)).filter((item) => item.zone_admitted === true && item.kind !== "memory" && typeof item.table === "string")
      .slice(0, MAX_TABLES).map((item) => ({ table: item.table as string, description: typeof item.description === "string" ? item.description : undefined }));
    const names = new Set(tables.map((table) => table.table));
    const fileListing = await read("context.files", bounds(request));
    if (!Array.isArray(fileListing.rows)) throw new BrowseError("ConsoleBrowseResponseMalformed");
    const files = fileListing.rows.filter((row): row is [string, string] =>
      Array.isArray(row) && typeof row[0] === "string" && typeof row[1] === "string" && names.has(row[0]))
      .slice(0, MAX_FILES).map(([table, path]) => ({ table, path, label: humanizeLabel(path) }));
    return { tables, files };
  }

  return {
    async discover(request: BrowseRequest) {
      const { tables, files } = await listed(request);
      const chips = tables.map((table) => ({ table: table.table, label: humanizeLabel(table.table), description: table.description }));
      const descriptions = await Promise.all(tables.map((table) => read("context.describe", { table: table.table, ...bounds(request) })));
      const insights = tables.flatMap((table, index) => {
        const count = descriptions[index].row_count;
        return typeof count === "number" && Number.isFinite(count) && count >= 0
          ? [{ table: table.table, label: humanizeLabel(table.table), rows: count }] : [];
      });
      return { chips, insights, files };
    },

    async preview(request: BrowseRequest & { path: string }) {
      const { files } = await listed(request);
      if (!files.some((file) => file.path === request.path)) throw new BrowseError("ConsoleGalleryPathUnlisted", request.path);
      const response = await read("context.file", { path: request.path, limit: MAX_PREVIEW_ROWS, ...bounds(request) });
      if (!Array.isArray(response.columns) || !Array.isArray(response.rows)) throw new BrowseError("ConsoleBrowseResponseMalformed");
      return { columns: response.columns.slice(0, 64), rows: response.rows.slice(0, MAX_PREVIEW_ROWS).map((row) =>
        Array.isArray(row) ? row.slice(0, 64) : []) };
    },
  };
}
