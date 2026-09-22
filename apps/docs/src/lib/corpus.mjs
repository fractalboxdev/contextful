// Reads the spec tree in place: routes, clause anchors and record ids.
// Imported by astro.config.mjs (markdown plugins) and by pages (navigation).
import { readdirSync, readFileSync, existsSync } from "node:fs";
import { basename, join, relative, resolve, sep } from "node:path";

// Anchored to the Astro project root (apps/docs), the same base the content
// collections use. `import.meta.url` is not a stable anchor: pages load this module
// from a bundled chunk whose location differs from src/lib.
export const SPEC_DIR = resolve(process.cwd(), "../../spec");
export const ADR_DIR = join(SPEC_DIR, "adr");
if (!existsSync(join(SPEC_DIR, "terms/contract.toml"))) {
  throw new Error(`spec/ not found at ${SPEC_DIR}: run the docs build from apps/docs`);
}

// A clause item: `- \`<subject>\` — <statement>`, its Why on an optional indented line.
export const CLAUSE_ITEM = /^- `([a-z0-9-]+)` — (.+)$/;

const mdFiles = (dir) =>
  existsSync(dir) ? readdirSync(dir).filter((f) => f.endsWith(".md")).sort() : [];

/** Record id of an ADR file: `P4`, `D07`, `A-store`. */
export const recordId = (file) => {
  const stem = basename(file, ".md");
  return stem.startsWith("A-") ? stem : stem.split("-")[0];
};

/** Site route for a Markdown file under spec/, or undefined outside it. */
export const routeFor = (absPath) => {
  const rel = relative(SPEC_DIR, absPath).split(sep).join("/");
  if (rel.startsWith("..") || !rel.endsWith(".md")) return undefined;
  const stem = rel.slice(0, -3);
  if (!stem.includes("/")) return `/spec/${stem}/`;
  const [dir, name, ...rest] = stem.split("/");
  if (rest.length === 0 && ["adr", "guide", "cards"].includes(dir)) return `/${dir}/${name}/`;
  return undefined;
};

/** `contract` and `owns` from a file's YAML front matter; the spec uses only a scalar and a string list. */
const frontMatter = (src) => {
  const block = /^---\n([\s\S]*?)\n---/.exec(src)?.[1] ?? "";
  const contract = /^contract:\s*(\S+)/m.exec(block)?.[1];
  const owns = [...block.matchAll(/^\s+-\s+(\S+)/gm)].map((m) => m[1]);
  return { contract, owns };
};

/** A statement as plain text: code ticks and emphasis dropped, `{{id}}` shown as its subject. */
const plainStatement = (s) =>
  s.replace(/\{\{[a-z0-9-]+\.[a-z0-9-]+\.([a-z0-9-]+)\}\}/g, "$1").replace(/[`*]/g, "").trim();

/**
 * The clause items of one contract file: under each `## <operation>`, the first
 * contiguous list after the lede. Fences, `###`+ subsections and `## Shapes` hold none.
 */
export const clauseItems = (src, contract) => {
  const out = [];
  let op;
  let fence = false;
  let state = "none"; // none → lede → list → done, per operation section
  for (const line of src.replace(/^---\n[\s\S]*?\n---\n/, "").split("\n")) {
    if (/^\s*(```|~~~)/.test(line)) fence = !fence;
    if (fence) continue;
    const h = /^(#{1,6}) (.+)$/.exec(line);
    if (h) {
      op = h[1].length === 2 && h[2].trim() !== "Shapes" ? h[2].trim() : undefined;
      state = "none";
      continue;
    }
    if (!op || state === "done") continue;
    const item = CLAUSE_ITEM.exec(line);
    if (item) {
      state = "list";
      out.push({ id: `${contract}.${op}.${item[1]}`, statement: plainStatement(item[2]) });
    } else if (state === "list" && !/^\s{2,}\S/.test(line)) {
      state = "done";
    }
  }
  return out;
};

/**
 * Clause id → `{ route, statement }`, `contract.operation` → route of the owning
 * section, record id → route. Read fresh on every call so `astro dev` tracks edits.
 */
export const corpusIndex = () => {
  const clauses = new Map();
  const operations = new Map();
  const files = mdFiles(SPEC_DIR).map((f) => {
    const src = readFileSync(join(SPEC_DIR, f), "utf8");
    return { route: routeFor(join(SPEC_DIR, f)), src, ...frontMatter(src) };
  });
  for (const { route, src, contract, owns } of files) {
    if (!contract) continue;
    for (const op of owns) operations.set(`${contract}.${op}`, `${route}#${op}`);
    for (const { id, statement } of clauseItems(src, contract)) {
      if (!clauses.has(id)) clauses.set(id, { route, statement });
    }
  }
  const records = new Map();
  for (const f of mdFiles(ADR_DIR)) records.set(recordId(f), routeFor(join(ADR_DIR, f)));
  return { clauses, operations, records };
};
