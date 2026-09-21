// Reads the spec tree in place: routes, clause anchors and record ids.
// Imported by astro.config.mjs (markdown plugins) and by pages (navigation).
import { readdirSync, readFileSync, existsSync } from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

export const SPEC_DIR = resolve(dirname(fileURLToPath(import.meta.url)), "../../../../spec");
export const ADR_DIR = join(SPEC_DIR, "adr");

const CLAUSE_ROW = /^\|\s*`([a-z0-9-]+\.[a-z0-9-]+\.[a-z0-9.-]+)`\s*\|/;

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
  if (stem.startsWith("adr/") && stem.split("/").length === 2) return `/${stem}/`;
  return undefined;
};

/** `contract` and `owns` from a file's YAML front matter; the spec uses only a scalar and a string list. */
const frontMatter = (src) => {
  const block = /^---\n([\s\S]*?)\n---/.exec(src)?.[1] ?? "";
  const contract = /^contract:\s*(\S+)/m.exec(block)?.[1];
  const owns = [...block.matchAll(/^\s+-\s+(\S+)/gm)].map((m) => m[1]);
  return { contract, owns };
};

/**
 * Clause id → route, `contract.operation` → route of the owning section, record id → route.
 * Read fresh on every call so `astro dev` tracks edits.
 */
export const corpusIndex = () => {
  const clauses = new Map();
  const operations = new Map();
  const files = mdFiles(SPEC_DIR).map((f) => {
    const src = readFileSync(join(SPEC_DIR, f), "utf8");
    return { route: routeFor(join(SPEC_DIR, f)), src, ...frontMatter(src) };
  });
  const owner = new Map();
  for (const { route, contract, owns } of files) {
    if (!contract) continue;
    for (const op of owns) {
      operations.set(`${contract}.${op}`, `${route}#${op}`);
      owner.set(`${contract}.${op}`, route);
    }
  }
  // A clause belongs to the file owning its operation; an example row elsewhere never claims it.
  for (const { route, src } of files) {
    for (const line of src.split("\n")) {
      const id = CLAUSE_ROW.exec(line)?.[1];
      if (!id) continue;
      const home = owner.get(id.split(".").slice(0, 2).join("."));
      if (home ? home === route : !clauses.has(id)) clauses.set(id, route);
    }
  }
  const records = new Map();
  for (const f of mdFiles(ADR_DIR)) records.set(recordId(f), routeFor(join(ADR_DIR, f)));
  return { clauses, operations, records };
};
