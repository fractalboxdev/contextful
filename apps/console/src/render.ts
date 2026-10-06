export type ResultRows = { columns: string[]; rows: unknown[][] };
export type Component = "metric.v1" | "line.v1" | "table.v1" | "bar.v1";
export type View = { component: Component; props: ResultRows; alternates?: Component[] };

function numeric(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

function day(value: unknown): boolean {
  if (typeof value !== "string" || !/^\d{4}-\d{2}-\d{2}$/.test(value)) return false;
  const parsed = new Date(`${value}T00:00:00Z`);
  return !Number.isNaN(parsed.getTime()) && parsed.toISOString().slice(0, 10) === value;
}

export function buildView(result: ResultRows, options: {
  origin?: "server" | "client" | "model";
  view?: unknown;
  hint?: { component: Component; columns: string[] };
} = {}): View {
  if (options.origin === "client" || options.origin === "model" || options.view !== undefined) {
    throw new Error("ConsoleViewNotServerBuilt");
  }
  const props = { columns: [...result.columns], rows: result.rows.map((row) => [...row]) };
  const oneMeasure = props.columns.length === 1 && props.rows.length === 1 && numeric(props.rows[0][0]);
  const dateIndex = props.columns.findIndex((column) => /^(date|day|.*_date|.*_day)$/i.test(column));
  const measureIndex = props.columns.findIndex((_, index) => index !== dateIndex && props.rows.every((row) => numeric(row[index])));
  const dates = dateIndex < 0 ? [] : props.rows.map((row) => row[dateIndex]);
  const line = dateIndex >= 0 && measureIndex >= 0 && props.rows.length >= 3 && dates.every(day) && new Set(dates).size === dates.length;
  const chosen: Component = oneMeasure ? "metric.v1" : line ? "line.v1" : "table.v1";
  const hint = options.hint;
  const binds = hint && hint.columns.length > 0 && hint.columns.every((column) => props.columns.includes(column));
  const component = binds && hint.component === "bar.v1" && chosen === "table.v1" ? "bar.v1" : chosen;
  return component === "table.v1" ? { component, props, alternates: ["bar.v1"] } : { component, props };
}

export function readView(view: { component: string; props: ResultRows }): View {
  const known = new Set<Component>(["metric.v1", "line.v1", "table.v1", "bar.v1"]);
  if (known.has(view.component as Component)) return view as View;
  return { component: "table.v1", props: view.props, alternates: ["bar.v1"] };
}

export function sanitizeView(view: View, redact: (value: string) => string): View | null {
  const { columns, rows } = view.props;
  if (!Array.isArray(columns) || !columns.every((column) => typeof column === "string") ||
      !Array.isArray(rows) || !rows.every((row) => Array.isArray(row) && row.length === columns.length)) return null;
  const seen = new WeakSet<object>();
  const walk = (value: unknown): unknown => {
    if (typeof value === "string") return redact(value);
    if (Array.isArray(value)) {
      if (seen.has(value)) throw new TypeError("cyclic view props");
      seen.add(value);
      const result = value.map(walk);
      seen.delete(value);
      return result;
    }
    if (value !== null && typeof value === "object") {
      if (seen.has(value)) throw new TypeError("cyclic view props");
      seen.add(value);
      const result = Object.fromEntries(Object.entries(value).map(([key, item]) => [key, walk(item)]));
      seen.delete(value);
      return result;
    }
    return value;
  };
  try {
    return { ...view, props: { columns: columns.map(redact), rows: rows.map((row) => row.map(walk)) } };
  } catch {
    return null;
  }
}

export function splitResult<TTrace, TSource>(result: ResultRows & { sources: TSource[]; trace: TTrace }): {
  grounding: ResultRows & { sources: TSource[] };
  trace: TTrace;
  view: View;
} {
  const { columns, rows, sources, trace } = result;
  return { grounding: { columns, rows, sources }, trace, view: buildView({ columns, rows }) };
}
