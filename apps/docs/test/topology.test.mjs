import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import test from "node:test";
import { createMarkdownProcessor } from "@astrojs/markdown-remark";
import { remarkTopologyTargets, remarkCorpus } from "../src/lib/markdown.mjs";
import { SPEC_DIR } from "../src/lib/corpus.mjs";

const processor = await createMarkdownProcessor({
  syntaxHighlight: false,
  remarkPlugins: [remarkTopologyTargets, remarkCorpus],
});
const render = (name, content = readFileSync(resolve(SPEC_DIR, name), "utf8")) =>
  processor.render(content, { fileURL: resolve(SPEC_DIR, name) });

test("topology contains generated provider tables inside the deployment section", async () => {
  const { code } = await render("01-topology.md");
  for (const text of ["worker-container-d1", "Fargate Linux tasks", "macOS host", "contextful pipeline serve process", "Workers V8 isolates", "Linux Container running contextful-full"]) {
    assert.ok(code.includes(text), text);
  }
  assert.ok(code.indexOf('id="deploy"') < code.indexOf('id="deployment-targets"'));
  assert.ok(code.indexOf('id="deployment-targets"') < code.indexOf('id="publish-hostname"'));
  assert.equal((code.match(/<h1\b/g) ?? []).length, 1);
  assert.equal((code.match(/<table>/g) ?? []).length, 3);
  assert.ok(code.includes('<h3 id="linux-reference"'));
  assert.ok(code.includes('id="macos-reference"'));
  assert.ok(code.includes('id="topology.deploy.unsupported-shape"'));
});

test("target links retain provider anchors on the topology route", async () => {
  const { code } = await render("10-store.md", "[targets](./targets.md) [Cloudflare](./targets.md#cloudflare)");
  assert.ok(code.includes('href="/docs/specs/contracts/01-topology/#deployment-targets"'));
  assert.ok(code.includes('href="/docs/specs/contracts/01-topology/#cloudflare"'));
});

test("other contracts contain no transcluded target tables", async () => {
  const { code } = await render("10-store.md");
  assert.ok(!code.includes('id="deployment-targets"'));
  assert.ok(!code.includes("worker-container-d1"));
});
