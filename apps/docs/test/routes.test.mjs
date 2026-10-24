import assert from "node:assert/strict";
import { resolve } from "node:path";
import test from "node:test";
import { createMarkdownProcessor } from "@astrojs/markdown-remark";
import { routeFor, SPEC_DIR } from "../src/lib/corpus.mjs";
import { remarkCorpus } from "../src/lib/markdown.mjs";

test("cards and decision records live inside the specification", () => {
  assert.equal(routeFor(resolve(SPEC_DIR, "cards/store.md")), "/docs/specs/cards/store/");
  assert.equal(routeFor(resolve(SPEC_DIR, "adr/A-store.md")), "/docs/specs/adr/A-store/");
  assert.equal(routeFor(resolve(SPEC_DIR, "10-store.md")), "/docs/specs/contracts/10-store/");
  assert.equal(routeFor(resolve(SPEC_DIR, "roadmap.md")), "/docs/specs/roadmap/");
  assert.equal(routeFor(resolve(SPEC_DIR, "guide/store.md")), "/docs/guide/store/");
  assert.equal(routeFor(resolve(SPEC_DIR, "../README.md")), undefined);
});

test("rendered references preserve clause anchors across the route move", async () => {
  const processor = await createMarkdownProcessor({ syntaxHighlight: false, remarkPlugins: [remarkCorpus] });
  const { code } = await processor.render(
    "[card](./cards/store.md) [record](./adr/A-store.md) {{store.fold.partial-snapshot}} A-store",
    { fileURL: resolve(SPEC_DIR, "10-store.md") },
  );
  assert.ok(code.includes('href="/docs/specs/cards/store/"'));
  assert.ok(code.includes('href="/docs/specs/adr/A-store/"'));
  assert.ok(code.includes('href="/docs/specs/contracts/10-store/#store.fold.partial-snapshot"'));
});
