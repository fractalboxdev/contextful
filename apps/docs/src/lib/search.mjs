// The site search index: Pagefind over the built pages, plus one record per clause carrying
// the errors it owns, so a search for an id or an error lands on the owning clause.
import { fileURLToPath } from "node:url";
import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import * as pagefind from "pagefind";
import { corpusIndex } from "./corpus.mjs";

/**
 * The records the index holds beside the pages: one per clause, under its id, statement and
 * the errors it owns. Pagefind keeps one record per URL, so an error rides its clause's record.
 */
export const searchRecords = () => {
  const { clauses, errors } = corpusIndex();
  const owned = new Map();
  for (const [name, { clause, gloss }] of errors) owned.set(clause, [...owned.get(clause) ?? [], { name, gloss }]);
  return [...clauses].map(([id, { route, statement }]) => {
    const raises = owned.get(id) ?? [];
    return {
      url: `${route}#${id}`,
      content: [id, statement, ...raises.flatMap(({ name, gloss }) => [name, gloss])].join(" "),
      language: "en",
      meta: { title: id, ...(raises.length ? { errors: raises.map(({ name }) => name).join(" ") } : {}) },
      filters: { contract: [id.split(".")[0]] },
    };
  });
};

const fail = (stage, errors) => {
  if (errors?.length) throw new Error(`Pagefind ${stage}: ${errors.join("; ")}`);
};

/** Every zero-byte file under `dir`, relative to it. */
export const emptyFiles = (dir) =>
  readdirSync(dir, { recursive: true })
    .map(String)
    .filter((rel) => statSync(join(dir, rel)).isFile() && statSync(join(dir, rel)).size === 0)
    .sort();

/**
 * Writes the index to `outputPath` and rewrites it while any file there is empty: Pagefind
 * can report a write complete with some fragments still unwritten, and a reader then
 * fails on a truncated archive. Three writes with an empty file refuse the build.
 */
export const writeComplete = async (index, outputPath) => {
  for (let write = 1; ; write += 1) {
    fail("write", (await index.writeFiles({ outputPath })).errors);
    const empty = emptyFiles(outputPath);
    if (!empty.length) return;
    if (write === 3) throw new Error(`Pagefind write: ${empty.length} empty file(s) after 3 writes, first ${empty[0]}`);
  }
};

/** Writes `dist/pagefind/` after the build: the pages' `data-pagefind-body`, then the records. */
export const searchIndex = {
  name: "search-index",
  hooks: {
    "astro:build:done": async ({ dir, logger }) => {
      const site = fileURLToPath(dir);
      const { index, errors } = await pagefind.createIndex({ forceLanguage: "en" });
      fail("index", errors);
      const pages = await index.addDirectory({ path: site });
      fail("pages", pages.errors);
      const records = searchRecords();
      for (const record of records) fail("record", (await index.addCustomRecord(record)).errors);
      await writeComplete(index, join(site, "pagefind"));
      await pagefind.close();
      logger.info(`indexed ${pages.page_count} pages and ${records.length} clauses`);
    },
  },
};
