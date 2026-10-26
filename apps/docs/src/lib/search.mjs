// The site search index: Pagefind over the built pages, plus one record per clause carrying
// the errors it owns, so a search for an id or an error lands on the owning clause.
import { fileURLToPath } from "node:url";
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
      fail("write", (await index.writeFiles({ outputPath: join(site, "pagefind") })).errors);
      await pagefind.close();
      logger.info(`indexed ${pages.page_count} pages and ${records.length} clauses`);
    },
  },
};
