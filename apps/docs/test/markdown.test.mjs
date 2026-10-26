import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import test from "node:test";
import { createMarkdownProcessor } from "@astrojs/markdown-remark";
import { ADR_DIR, corpusIndex, readContract, readLock, routeFor, SPEC_DIR } from "../src/lib/corpus.mjs";
import { remarkCorpus } from "../src/lib/markdown.mjs";

const fixture = (name) => readFileSync(join("test/fixtures", name), "utf8");

const render = async (source, file, options) => {
  const processor = await createMarkdownProcessor({ syntaxHighlight: false, remarkPlugins: [[remarkCorpus, options]] });
  return (await processor.render(source, { fileURL: resolve(SPEC_DIR, file) })).code;
};

const clauseItem = (html, id) => new RegExp(`<li class="clause" id="${id.replaceAll(".", "\\.")}">[\\s\\S]*?</li>`).exec(html)?.[0];

test("the site reads the same clause ids as the lock file", () => {
  const { clauses } = corpusIndex();
  assert.deepEqual([...clauses.keys()].sort(), readLock().clauses.map((c) => c.id).sort());
});

test("a group heading opens a lede and clause list whose ids omit the group", async () => {
  const source = fixture("grouped-operation.md");
  assert.deepEqual(readContract(source, "store").clauses.map((c) => c.id), [
    "store.reserve.column-name", "store.reserve.table-name", "store.reserve.commit-wait", "store.reserve.commit-order",
  ]);
  const html = await render(source, "10-store.md");
  assert.ok(html.includes('<h3 class="clause-group" id="reserve--commit">commit</h3>'));
  assert.ok(html.includes('<p class="lede group-lede">'));
  for (const id of ["store.reserve.column-name", "store.reserve.commit-wait", "store.reserve.commit-order"]) {
    assert.ok(clauseItem(html, id), id);
  }
  assert.equal(html.match(/<ul class="clauses"/g).length, 2);
});

test("a Why folds into details and every reference carries its statement preview", async () => {
  const html = await render(fixture("grouped-operation.md"), "10-store.md");
  assert.equal(html.match(/<details class="clause-why">/g).length, 2);
  assert.ok(!clauseItem(html, "store.reserve.table-name").includes("clause-why"));
  const refs = [...html.matchAll(/<a\b[^>]*class="(?:clause-ref|error-ref)[^"]*"[^>]*>([\s\S]*?)<\/a>/g)];
  assert.ok(refs.length >= 4);
  for (const [ref, inner] of refs) {
    assert.ok(inner.includes('<span class="clause-preview" aria-hidden="true" data-pagefind-ignore="">'), ref);
    assert.ok(/\btitle="[^"]+ — /.test(ref), `title fallback: ${ref}`);
  }
});

test("a two-segment pointer renders the operation name linked to its section", async () => {
  const html = await render(fixture("grouped-operation.md"), "10-store.md");
  const link = /<a href="\/docs\/specs\/contracts\/10-store\/#fold"[^>]*class="clause-ref operation-ref"><code>fold<\/code>/.exec(html);
  assert.ok(link, "operation link");
  assert.ok(!html.includes("{{store.fold}}"));
});

test("an error identifier links to its owning clause everywhere but inside that clause", async () => {
  const html = await render(fixture("grouped-operation.md"), "10-store.md");
  assert.ok(clauseItem(html, "store.reserve.column-name").includes("raises <code>StoreReservedColumnName</code>."));
  assert.ok(clauseItem(html, "store.reserve.commit-order").includes(
    'href="/docs/specs/contracts/10-store/#store.reserve.column-name"',
  ));
  assert.ok(/class="error-ref"><code>StoreReservedColumnName<\/code>/.test(clauseItem(html, "store.reserve.commit-order")));
});

test("a section-qualified record id links to the ADR section named by its slug", async () => {
  const clauses = await render(fixture("grouped-operation.md"), "10-store.md");
  assert.ok(clauses.includes('<a href="/docs/specs/adr/A-store/#lease" class="record-ref">A-store#lease</a>'));
  const adr = await render(fixture("decision-sections.md"), join(ADR_DIR, "A-store.md"));
  assert.ok(adr.includes('<h2 id="lease">lease — A lease is a fenced compare-and-swap on the bucket</h2>'));
  assert.ok(adr.includes('id="an-unslugged-decision-keeps-its-generated-anchor"'));
  assert.ok(adr.includes('<a href="#lease" class="record-ref">A-store#lease</a>'));
});

test("a clause's pins from the lock render in its margin beside the id", async () => {
  const lock = { ...readLock(), pins: JSON.parse(fixture("lock-pins.json")) };
  const html = await render(fixture("grouped-operation.md"), "10-store.md", { lock });
  const margin = /<span class="clause-margin">([\s\S]*?)<\/span><span class="clause-statement">/.exec(clauseItem(html, "store.reserve.commit-wait"))?.[1];
  assert.ok(margin?.includes('class="clause-pins"'));
  assert.ok(margin.includes('href="https://github.com/fractalboxdev/contextful/blob/main/crates/contextful-context/tests/integration/commit.rs#L42"'));
  assert.ok(margin.includes('class="clause-pin pin-theorem"'));
  assert.ok(!clauseItem(html, "store.reserve.column-name").includes("clause-pins"));
  // A pin whose path resolves to nothing names spec/pins.toml at line 0, and links to no line.
  assert.ok(clauseItem(html, "store.reserve.table-name").includes('href="https://github.com/fractalboxdev/contextful/blob/main/spec/pins.toml"'));
  // The verdict stays off the page: the site renders where a clause is pinned, not its state.
  assert.ok(!html.includes("performed") && !html.includes("broken"));
  const unpinned = await render(fixture("grouped-operation.md"), "10-store.md", { lock: { clauses: [] } });
  assert.ok(!unpinned.includes("clause-pins"));
});

test("the pins `contextful-spec state` writes render as one link per site", async () => {
  const pins = {
    "store.reserve.commit-wait": {
      sites: ["crates/contextful-context/tests/integration/commit.rs::commit_waits_for_the_table_lock", "formal/protocol/Protocol/Invariants.lean::commit_order"],
      verdict: "performed",
      proved: true,
    },
  };
  const html = await render(fixture("grouped-operation.md"), "10-store.md", { lock: { ...readLock(), pins } });
  const item = clauseItem(html, "store.reserve.commit-wait");
  assert.ok(item.includes('href="https://github.com/fractalboxdev/contextful/blob/main/crates/contextful-context/tests/integration/commit.rs"'));
  assert.ok(item.includes('class="clause-pin pin-theorem"'));
});

test("a glossary under spec/ routes to its own page and links its pointers", async () => {
  assert.equal(routeFor(resolve(SPEC_DIR, "glossary.md")), "/docs/specs/glossary/");
  const html = await render(fixture("glossary.md"), "glossary.md");
  assert.ok(html.includes('<h2 id="fence">fence</h2>'));
  assert.ok(/<a href="\/docs\/specs\/contracts\/10-store\/#lease"[^>]*class="clause-ref operation-ref"><code>lease<\/code>/.test(html));
  assert.ok(!html.includes('class="clause"'));
});
