import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";
import { legacyRedirects, sitePaths } from "../src/lib/corpus.mjs";

const origin = "https://contextful.fractalbox.dev";
const fileFor = (path) => join("dist", path.endsWith("/") ? `${path}index.html` : path);
const htmlFor = (path) => readFileSync(fileFor(path), "utf8");

test("the homepage introduces the product and docs have a separate navigation layout", () => {
  const home = htmlFor("/");
  const docs = htmlFor("/docs/");
  assert.ok(home.includes('class="landing"'));
  assert.ok(home.includes("Your data."));
  assert.ok(home.includes('href="/docs/"'));
  assert.ok(docs.includes('class="sidebar"'));
  assert.ok(docs.includes('href="/docs/specs/cards/"'));
  assert.ok(docs.includes('href="/docs/specs/adr/"'));
  assert.ok(!home.includes('class="sidebar"'));
});

test("every canonical page and its internal links resolve in the production build", () => {
  const paths = sitePaths();
  assert.ok(paths.length > 60);
  const failures = new Set();
  for (const path of paths) {
    assert.ok(existsSync(fileFor(path)), path);
    const html = htmlFor(path);
    assert.ok(html.includes(`rel="canonical" href="${origin}${path}"`), `canonical: ${path}`);
    for (const [, href] of html.matchAll(/\bhref="([^"]+)"/g)) {
      const url = new URL(href.replaceAll("&amp;", "&"), `${origin}${path}`);
      if (url.origin !== origin) continue;
      if (!existsSync(fileFor(url.pathname))) {
        failures.add(`${path} → ${url.pathname}`);
      } else if (url.hash && url.pathname.endsWith("/")) {
        const id = decodeURIComponent(url.hash.slice(1));
        if (!htmlFor(url.pathname).includes(`id="${id}"`)) failures.add(`${path} → ${url.pathname}#${id}`);
      }
    }
  }
  assert.deepEqual([...failures], []);
});

test("redirects, sitemap and machine-readable exports cover the canonical routes", () => {
  const rules = readFileSync("dist/_redirects", "utf8");
  const sitemap = readFileSync("dist/sitemap.xml", "utf8");
  const index = readFileSync("dist/llms.txt", "utf8");
  const full = readFileSync("dist/llms-full.txt", "utf8");
  const sources = rules.trim().split("\n").map((line) => line.split(/\s+/)[0]);
  assert.ok(sources.length > 60);
  for (const source of sources) assert.ok(source.startsWith("/"), `Workers require relative redirect sources: ${source}`);
  for (const [from, to] of Object.entries(legacyRedirects())) assert.ok(rules.includes(`${from} ${origin}${to} 301`), from);
  for (const path of sitePaths()) assert.ok(sitemap.includes(`<loc>${origin}${path}</loc>`), path);
  assert.ok(index.includes(`${origin}/docs/specs/cards/store/`));
  assert.ok(index.includes(`${origin}/docs/specs/adr/A-store/`));
  assert.ok(full.includes("store.fold.partial-snapshot"));
  assert.ok(full.includes(`Source: ${origin}/docs/specs/contracts/01-topology/#deployment-targets`));
  assert.ok(full.includes("Fargate Linux tasks"), "deployment target tables appear in the full Markdown export");
  assert.ok(readFileSync("dist/robots.txt", "utf8").includes(`${origin}/sitemap.xml`));
});
