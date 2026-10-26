// Reads the spec tree in place: routes, clause anchors and record ids.
// Imported by astro.config.mjs (markdown plugins) and by pages (navigation).
import { readdirSync, readFileSync, existsSync, statSync } from "node:fs";
import { basename, join, relative, resolve, sep } from "node:path";
import { parse } from "smol-toml";

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

/** The contract registry supplies metadata and canonical contract routes. */
export const contractsByFile = () => {
  const out = new Map();
  const { contract } = parse(readFileSync(join(SPEC_DIR, "terms/contract.toml"), "utf8"));
  for (const [name, entry] of Object.entries(contract)) {
    for (const file of entry.files) {
      out.set(file.replace(/^spec\//, "").replace(/\.md$/, ""), {
        contract: name, title: entry.title, gloss: entry.gloss,
      });
    }
  }
  return out;
};

/** Canonical routes share one mapping across navigation, Markdown and generated pages. */
export const collectionRoute = (collection, id) => {
  if (collection === "spec") {
    if (id === "targets") return `${collectionRoute("spec", "01-topology")}#deployment-targets`;
    return `/docs/specs/${contractsByFile().has(id) ? "contracts/" : ""}${id}/`;
  }
  return collection === "guide" ? `/docs/guide/${id}/` : `/docs/specs/${collection}/${id}/`;
};

/**
 * Every former content URL resolves to the corresponding canonical page.
 * @returns {Record<string, string>}
 */
export const legacyRedirects = () => Object.fromEntries([
  ["/docs/guide/", "/docs/"],
  ...["spec", "guide", "cards", "adr"].flatMap((collection) =>
    mdFiles(collection === "spec" ? SPEC_DIR : join(SPEC_DIR, collection)).map((file) => {
      const id = basename(file, ".md");
      return [`/${collection}/${id}/`, collectionRoute(collection, id)];
    }),
  ),
]);

export const sitePaths = () => [...new Set([
  "/", "/docs/", "/docs/specs/", "/docs/specs/contracts/", "/docs/specs/cards/", "/docs/specs/adr/",
  ...Object.values(legacyRedirects()).map((route) => route.split("#")[0]),
])];

/** Machine-readable exports read the same Markdown as the content collections. */
export const sourceDocuments = () => ["spec", "guide", "cards", "adr"].flatMap((collection) => {
  const directory = collection === "spec" ? SPEC_DIR : join(SPEC_DIR, collection);
  return mdFiles(directory).map((file) => ({
    route: collectionRoute(collection, basename(file, ".md")),
    body: readFileSync(join(directory, file), "utf8"),
  }));
});

/** Site route for a Markdown file under spec/, or undefined outside it. */
export const routeFor = (absPath) => {
  const rel = relative(SPEC_DIR, absPath).split(sep).join("/");
  if (rel.startsWith("..") || !rel.endsWith(".md")) return undefined;
  const stem = rel.slice(0, -3);
  if (!stem.includes("/")) return collectionRoute("spec", stem);
  const [dir, name, ...rest] = stem.split("/");
  if (rest.length === 0 && ["adr", "guide", "cards"].includes(dir)) return collectionRoute(dir, name);
  return undefined;
};

/** `contract` and `owns` from a file's YAML front matter; the spec uses only a scalar and a string list. */
const frontMatter = (src) => {
  const block = /^---\n([\s\S]*?)\n---/.exec(src)?.[1] ?? "";
  const contract = /^contract:\s*(\S+)/m.exec(block)?.[1];
  const owns = [...block.matchAll(/^\s+-\s+(\S+)/gm)].map((m) => m[1]);
  return { contract, owns };
};

/** A statement as plain text: code ticks and emphasis dropped, a `{{pointer}}` shown as its last segment. */
const plainStatement = (s) =>
  s.replace(/\{\{(?:[a-z0-9-]+\.)+([a-z0-9-]+)\}\}/g, "$1").replace(/[`*]/g, "").trim();

/**
 * The clause items and operation ledes of one contract file. Under each `## <operation>`,
 * the lede is the first paragraph and the clauses the first contiguous list after it; each
 * `### <group>` inside the operation opens another lede and list whose ids omit the group.
 * Fences, `####`+ subsections and `## Shapes` hold none.
 */
export const readContract = (src, contract) => {
  const clauses = [];
  const ledes = new Map();
  let op;
  let active;
  let fence = false;
  let state = "none"; // none → list → done, per operation or group
  let lede; // the lede's lines while the operation's first paragraph is open
  for (const line of src.replace(/^---\n[\s\S]*?\n---\n/, "").split("\n")) {
    if (/^\s*(```|~~~)/.test(line)) fence = !fence;
    if (fence) continue;
    const h = /^(#{1,6}) (.+)$/.exec(line);
    if (h) {
      const depth = h[1].length;
      if (depth <= 2) op = depth === 2 && h[2].trim() !== "Shapes" ? h[2].trim() : undefined;
      active = depth <= 3 ? op : undefined;
      lede = depth === 2 && op ? [] : undefined;
      state = "none";
      continue;
    }
    if (lede) {
      if (line.trim() && !/^\s*([-*|>]|\d+\.)\s/.test(line)) {
        lede.push(line.trim());
        continue;
      }
      if (lede.length) ledes.set(op, plainStatement(lede.join(" ")));
      if (line.trim() || lede.length) lede = undefined;
    }
    if (!active || state === "done") continue;
    const item = CLAUSE_ITEM.exec(line);
    if (item) {
      state = "list";
      clauses.push({ id: `${contract}.${active}.${item[1]}`, statement: plainStatement(item[2]) });
    } else if (state === "list" && !/^\s{2,}\S/.test(line)) {
      state = "done";
    }
  }
  return { clauses, ledes };
};

const TERMS_DIR = join(SPEC_DIR, "terms");

/** Error identifier → `{ clause, gloss }`, from every fragment's `[error]` table. */
export const registeredErrors = () => {
  const out = new Map();
  for (const f of readdirSync(TERMS_DIR).filter((f) => f.endsWith(".toml")).sort()) {
    const { error } = parse(readFileSync(join(TERMS_DIR, f), "utf8"));
    for (const [name, entry] of Object.entries(error ?? {})) {
      if (typeof entry?.clause === "string") out.set(name, { clause: entry.clause, gloss: entry.gloss ?? "" });
    }
  }
  return out;
};

const LOCK = join(SPEC_DIR, "spec.lock.json");
let lockCache;

/** `spec/spec.lock.json`, re-read when its modification time changes; `{}` when absent. */
export const readLock = () => {
  if (!existsSync(LOCK)) return {};
  const mtime = statSync(LOCK).mtimeMs;
  if (lockCache?.mtime !== mtime) lockCache = { mtime, lock: JSON.parse(readFileSync(LOCK, "utf8")) };
  return lockCache.lock;
};

export const REPOSITORY = "https://github.com/fractalboxdev/contextful";

/**
 * Clause id → its pins from the lock's `pins` key, each `{ kind, label, href }`; an empty
 * map while the lock carries none.
 */
export const lockPins = (lock) => {
  const out = new Map();
  for (const [id, entries] of Object.entries(lock.pins ?? {})) {
    // `contextful-spec state` writes `{ sites: ["<path>::<name>"], verdict, proved }`.
    const list = Array.isArray(entries?.sites)
      ? entries.sites.map((site) => {
          const [path, name] = site.split("::");
          return { kind: path.endsWith(".lean") ? "theorem" : path.endsWith(".toml") ? "item" : "test", path, name };
        })
      : [entries].flat();
    out.set(id, list.map((pin) => {
      const { kind = "test", path, file = path, name, test, theorem, line } = typeof pin === "string" ? { name: pin } : pin;
      const target = name ?? test ?? theorem ?? "";
      return {
        kind,
        label: [file, target].filter(Boolean).join(" — "),
        href: file ? `${REPOSITORY}/blob/main/${file}${line ? `#L${line}` : ""}` : undefined,
      };
    }));
  }
  return out;
};

/**
 * Clause id → `{ route, statement, pins }`, `contract.operation` → `{ url, lede }` of the
 * owning section, record id → route, error identifier → `{ clause, gloss }`. Read fresh on
 * every call so `astro dev` tracks edits. `lock` stands in for `spec/spec.lock.json`.
 */
export const corpusIndex = (lock = readLock()) => {
  const clauses = new Map();
  const operations = new Map();
  const pins = lockPins(lock);
  const files = mdFiles(SPEC_DIR).map((f) => {
    const src = readFileSync(join(SPEC_DIR, f), "utf8");
    return { route: routeFor(join(SPEC_DIR, f)), src, ...frontMatter(src) };
  });
  for (const { route, src, contract, owns } of files) {
    if (!contract) continue;
    const { clauses: items, ledes } = readContract(src, contract);
    for (const op of owns) operations.set(`${contract}.${op}`, { url: `${route}#${op}`, lede: ledes.get(op) ?? "" });
    for (const { id, statement } of items) {
      if (!clauses.has(id)) clauses.set(id, { route, statement, pins: pins.get(id) ?? [] });
    }
  }
  const records = new Map();
  for (const f of mdFiles(ADR_DIR)) records.set(recordId(f), routeFor(join(ADR_DIR, f)));
  return { clauses, operations, records, errors: registeredErrors() };
};
