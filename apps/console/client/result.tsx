import { useMemo, useState } from "react";
import { Bar, BarChart, CartesianGrid, Line, LineChart, Tooltip, XAxis, YAxis } from "recharts";
import { BarChart3Icon, DownloadIcon, TableIcon } from "lucide-react";
import {
  Badge, Button, Card, CardContent, ChartContainer, chartTooltipStyle, Table, TableBody, TableCell, TableHead, TableHeader, TableRow,
  Tabs, TabsContent, TabsList, TabsTrigger,
} from "./ui.tsx";

export type Rows = { columns: string[]; rows: unknown[][] };
export type Widget = { component: string; props: Rows; alternates?: string[] };

type ColumnKind = "number" | "date" | "boolean" | "category" | "text";
type ColumnMeta = { name: string; index: number; kind: ColumnKind; isMeasure: boolean; extent?: { min: number; max: number } };

const IDENTIFIERS = ["id", "ids", "symbol", "symbols", "code", "codes", "zip", "year", "qtr", "quarter", "month", "day"];
const DATE_RE = /^\d{4}-\d{2}-\d{2}([T ]\d{2}:\d{2}(:\d{2})?(\.\d+)?(Z|[+-]\d{2}:?\d{2})?)?$/;
const integers = new Intl.NumberFormat("en-US");

function empty(value: unknown): boolean {
  return value === null || value === undefined || value === "";
}

export function asNumber(value: unknown): number | null {
  if (typeof value === "number") return Number.isFinite(value) ? value : null;
  if (typeof value === "string" && value.trim() !== "") {
    const parsed = Number(value.trim());
    return Number.isFinite(parsed) ? parsed : null;
  }
  return null;
}

function kindOf(cells: unknown[]): ColumnKind {
  if (cells.length === 0) return "text";
  if (cells.every((cell) => typeof cell === "boolean" || cell === "true" || cell === "false")) return "boolean";
  if (cells.every((cell) => typeof cell === "string" && DATE_RE.test(cell.trim()))) return "date";
  if (cells.every((cell) => asNumber(cell) !== null)) return "number";
  const distinct = new Set(cells.map(String));
  return distinct.size <= 6 && Math.max(...cells.map((cell) => String(cell).length)) <= 16 ? "category" : "text";
}

function identifier(name: string): boolean {
  const lower = name.trim().toLowerCase();
  return IDENTIFIERS.some((entry) => lower === entry || lower.endsWith(`_${entry}`));
}

export function inferColumns(columns: string[], rows: unknown[][]): ColumnMeta[] {
  return columns.map((name, index) => {
    const cells = rows.map((row) => row[index]).filter((cell) => !empty(cell));
    const kind = kindOf(cells);
    const meta: ColumnMeta = { name, index, kind, isMeasure: kind === "number" && !identifier(name) };
    if (meta.isMeasure) {
      const numbers = cells.map(asNumber).filter((value): value is number => value !== null);
      if (numbers.length) meta.extent = { min: Math.min(...numbers, 0), max: Math.max(...numbers) };
    }
    return meta;
  });
}

export function formatCell(value: unknown, kind: ColumnKind): string {
  if (empty(value)) return "";
  if (kind === "number") {
    const parsed = asNumber(value);
    if (parsed === null) return String(value);
    return Number.isInteger(parsed) ? integers.format(parsed) : String(parsed);
  }
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}

export function pickChart(metas: ColumnMeta[], rowCount: number): { labelIndex: number; valueIndex: number } | null {
  if (rowCount < 1 || rowCount > 50) return null;
  const value = metas.find((meta) => meta.isMeasure);
  if (!value) return null;
  const other = (kinds: ColumnKind[]) => metas.find((meta) => meta.index !== value.index && kinds.includes(meta.kind) && !meta.isMeasure);
  const label = other(["number"]) ?? other(["category"]) ?? other(["date"]) ?? other(["text"]) ?? metas.find((meta) => meta.index !== value.index);
  return label ? { labelIndex: label.index, valueIndex: value.index } : null;
}

export function toCsv(columns: string[], rows: unknown[][]): string {
  const escape = (value: unknown) => {
    if (value === null || value === undefined) return "";
    const text = typeof value === "object" ? JSON.stringify(value) : String(value);
    return /[",\n]/.test(text) ? `"${text.replace(/"/g, '""')}"` : text;
  };
  return `${columns.map(escape).join(",")}\n${rows.map((row) => row.map(escape).join(",")).join("\n")}\n`;
}

function MeasureCell({ value, meta }: { value: unknown; meta: ColumnMeta }) {
  const parsed = asNumber(value);
  const max = meta.extent?.max ?? 0;
  const width = max > 0 && parsed !== null ? Math.max(2, Math.min(100, (parsed / max) * 100)) : 0;
  return (
    <div className="flex items-center justify-end gap-2">
      <span className="font-mono tabular-nums">{formatCell(value, "number")}</span>
      <span className="h-1.5 w-16 shrink-0 overflow-hidden rounded-full bg-muted" aria-hidden>
        <span className="block h-full rounded-full bg-primary/70" style={{ width: `${width}%` }} />
      </span>
    </div>
  );
}

function Cell({ value, meta }: { value: unknown; meta: ColumnMeta }) {
  if (empty(value)) return <span className="text-muted-foreground/40">—</span>;
  if (meta.kind === "category") return <Badge variant="secondary">{String(value)}</Badge>;
  if (meta.kind === "boolean") return <Badge variant={String(value) === "true" ? "positive" : "muted"}>{String(value)}</Badge>;
  if (meta.isMeasure) return <MeasureCell value={value} meta={meta} />;
  if (meta.kind === "number") return <span className="font-mono tabular-nums">{String(value)}</span>;
  if (meta.kind === "date") return <span className="font-mono text-muted-foreground">{formatCell(value, "date")}</span>;
  const text = formatCell(value, "text");
  return <span className="block max-w-[44ch] truncate" title={text}>{text}</span>;
}

type Sort = { index: number; direction: "asc" | "desc" };

function compare(left: unknown, right: unknown, meta: ColumnMeta): number {
  if (meta.kind === "number" || meta.kind === "date") {
    const value = (cell: unknown) => {
      const parsed = meta.kind === "number" ? asNumber(cell) : Date.parse(String(cell ?? ""));
      return parsed === null || Number.isNaN(parsed) ? -Infinity : parsed;
    };
    return value(left) - value(right);
  }
  return String(left ?? "").localeCompare(String(right ?? ""));
}

function DataTable({ columns, rows, metas }: Rows & { metas: ColumnMeta[] }) {
  const [sort, setSort] = useState<Sort | null>(null);
  const sorted = useMemo(() => {
    if (!sort) return rows;
    const factor = sort.direction === "asc" ? 1 : -1;
    return [...rows].sort((left, right) => compare(left[sort.index], right[sort.index], metas[sort.index]) * factor);
  }, [rows, sort, metas]);
  const toggle = (index: number) => setSort((current) =>
    current?.index === index ? (current.direction === "asc" ? { index, direction: "desc" } : null) : { index, direction: "asc" });
  const download = () => {
    const url = URL.createObjectURL(new Blob([toCsv(columns, rows)], { type: "text/csv;charset=utf-8" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = "result.csv";
    link.click();
    URL.revokeObjectURL(url);
  };
  return (
    <div className="space-y-2">
      <div className="overflow-hidden rounded-xl border border-border">
        <Table>
          <TableHeader className="bg-muted/40">
            <TableRow className="hover:bg-transparent">
              {columns.map((column, index) => {
                const measure = metas[index]?.isMeasure;
                const active = sort?.index === index;
                return (
                  <TableHead key={`${column}-${index}`} className={measure ? "text-right" : undefined}>
                    <button type="button" onClick={() => toggle(index)}
                      className={`inline-flex items-center gap-1 transition-colors hover:text-foreground ${measure ? "flex-row-reverse" : ""} ${active ? "text-foreground" : ""}`}>
                      {column}
                      <span className="text-[10px] text-muted-foreground/70">{active ? (sort?.direction === "asc" ? "▲" : "▼") : "↕"}</span>
                    </button>
                  </TableHead>
                );
              })}
            </TableRow>
          </TableHeader>
          <TableBody>
            {sorted.map((row, rowIndex) => (
              <TableRow key={rowIndex}>
                {row.map((cell, index) => (
                  <TableCell key={index} className={`${metas[index]?.isMeasure ? "text-right" : ""} ${metas[index]?.kind === "text" ? "text-muted-foreground" : "text-foreground"}`}>
                    {metas[index] ? <Cell value={cell} meta={metas[index]} /> : String(cell ?? "")}
                  </TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
      <div className="flex items-center justify-between px-1 text-xs text-muted-foreground">
        <span>{rows.length} row{rows.length === 1 ? "" : "s"}</span>
        <Button variant="ghost" size="sm" onClick={download} className="text-muted-foreground">
          <DownloadIcon className="size-3.5" /> CSV
        </Button>
      </div>
    </div>
  );
}

function BarView({ columns, rows, pick }: Rows & { pick: { labelIndex: number; valueIndex: number } }) {
  const data = useMemo(() => rows.slice(0, 50).map((row) => ({ label: String(row[pick.labelIndex] ?? "—"), value: asNumber(row[pick.valueIndex]) ?? 0 })),
    [rows, pick.labelIndex, pick.valueIndex]);
  return (
    <div className="rounded-xl border border-border bg-card/40 p-3">
      <ChartContainer height={Math.max(200, Math.min(440, data.length * 30 + 48))}>
        <BarChart data={data} layout="vertical" margin={{ left: 4, right: 20, top: 4, bottom: 4 }}>
          <CartesianGrid horizontal={false} stroke="var(--color-border)" strokeOpacity={0.4} />
          <XAxis type="number" stroke="var(--color-muted-foreground)" fontSize={11} tickLine={false} axisLine={false} />
          <YAxis type="category" dataKey="label" width={96} stroke="var(--color-muted-foreground)" fontSize={11} tickLine={false} axisLine={false} />
          <Tooltip {...chartTooltipStyle} />
          <Bar dataKey="value" name={columns[pick.valueIndex]} fill="var(--color-chart-1)" radius={[0, 4, 4, 0]} />
        </BarChart>
      </ChartContainer>
    </div>
  );
}

function LineView({ columns, rows }: Rows) {
  const metas = inferColumns(columns, rows);
  const date = metas.findIndex((meta) => meta.kind === "date");
  const measure = metas.findIndex((meta) => meta.index !== date && meta.isMeasure);
  if (date < 0 || measure < 0) return <ColumnarResult columns={columns} rows={rows} />;
  const data = rows.map((row) => ({ label: String(row[date]), value: asNumber(row[measure]) ?? 0 }));
  return (
    <div className="rounded-xl border border-border bg-card/40 p-3">
      <ChartContainer height={240}>
        <LineChart data={data} margin={{ left: 4, right: 20, top: 8, bottom: 4 }}>
          <CartesianGrid vertical={false} stroke="var(--color-border)" strokeOpacity={0.4} />
          <XAxis dataKey="label" stroke="var(--color-muted-foreground)" fontSize={11} tickLine={false} axisLine={false} />
          <YAxis stroke="var(--color-muted-foreground)" fontSize={11} tickLine={false} axisLine={false} width={48} />
          <Tooltip {...chartTooltipStyle} />
          <Line type="monotone" dataKey="value" name={columns[measure]} stroke="var(--color-chart-1)" strokeWidth={2} dot={false} />
        </LineChart>
      </ChartContainer>
    </div>
  );
}

function MetricView({ columns, rows }: Rows) {
  return (
    <Card className="w-fit min-w-48">
      <CardContent className="pt-4">
        <p className="text-xs tracking-wider text-muted-foreground uppercase">{columns[0]}</p>
        <p className="mt-1 font-mono text-3xl font-semibold tabular-nums">{formatCell(rows[0]?.[0], "number")}</p>
      </CardContent>
    </Card>
  );
}

/** A columnar result: a Table/Chart switch when a measure has a label column, else the table. */
export function ColumnarResult({ columns, rows, chart = false }: Rows & { chart?: boolean }) {
  const metas = useMemo(() => inferColumns(columns, rows), [columns, rows]);
  const pick = useMemo(() => pickChart(metas, rows.length), [metas, rows.length]);
  if (rows.length === 0) {
    return <p className="rounded-xl border border-dashed border-border p-6 text-center text-sm text-muted-foreground">0 rows</p>;
  }
  if (!pick) return <DataTable columns={columns} rows={rows} metas={metas} />;
  return (
    <Tabs defaultValue={chart ? "chart" : "table"}>
      <TabsList>
        <TabsTrigger value="table"><TableIcon className="size-3.5" /> Table</TabsTrigger>
        <TabsTrigger value="chart"><BarChart3Icon className="size-3.5" /> Chart</TabsTrigger>
      </TabsList>
      <TabsContent value="table"><DataTable columns={columns} rows={rows} metas={metas} /></TabsContent>
      <TabsContent value="chart"><BarView columns={columns} rows={rows} pick={pick} /></TabsContent>
    </Tabs>
  );
}

/** A server-built view; an unknown component renders as a table. */
export function WidgetView({ widget }: { widget: Widget }) {
  const props = widget.props;
  if (!props || !Array.isArray(props.columns) || !Array.isArray(props.rows)) return null;
  if (widget.component === "metric.v1") return <MetricView {...props} />;
  if (widget.component === "line.v1") return <LineView {...props} />;
  return <ColumnarResult {...props} chart={widget.component === "bar.v1"} />;
}
