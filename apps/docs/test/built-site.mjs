import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import test from "node:test";
import { gunzipSync } from "node:zlib";
import { legacyRedirects, readLock, registeredErrors, routeFor, sitePaths, SPEC_DIR } from "../src/lib/corpus.mjs";

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

test("docs and individual guides share stable navigation with a separate page TOC", () => {
  const sidebarLinks = (html) => {
    const sidebar = /<nav\b[^>]*class="sidebar"[^>]*>([\s\S]*?)<\/nav>/.exec(html)?.[1];
    assert.ok(sidebar, "documentation sidebar exists");
    return [...sidebar.matchAll(/<a\b[^>]*href="([^"]+)"[^>]*>([\s\S]*?)<\/a>/g)]
      .map(([, href, label]) => ({ href, label: label.replace(/<[^>]+>/g, "").trim() }));
  };
  const docs = htmlFor("/docs/");
  const expected = sidebarLinks(docs);
  assert.ok(expected.some(({ href }) => href === "/docs/guide/assurance/"));
  assert.ok(expected.some(({ href }) => href === "/docs/guide/store/"));
  const toc = /<aside\b[^>]*class="toc"[^>]*>([\s\S]*?)<\/aside>/.exec(docs)?.[1];
  assert.ok(toc?.includes('href="#choose-a-guide"'));
  for (const path of sitePaths().filter((path) => path.startsWith("/docs/"))) {
    const html = htmlFor(path);
    assert.deepEqual(sidebarLinks(html), expected, path);
    assert.equal(html.includes('href="/docs/guide/"'), false, `retired index link: ${path}`);
  }
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

const lock = readLock();
const pages = () => [...sitePaths().filter((path) => path.startsWith("/docs/")), "/docs/search/"];

test("each contract page folds every Why into details and addresses every clause", () => {
  const byRoute = Map.groupBy(lock.clauses, (clause) => routeFor(resolve(SPEC_DIR, "..", clause.file)));
  assert.ok(byRoute.size > 20);
  for (const [route, clauses] of byRoute) {
    const html = htmlFor(route);
    assert.equal(html.match(/<li class="clause" id="/g)?.length ?? 0, clauses.length, `clauses: ${route}`);
    assert.equal(html.match(/<details class="clause-why">/g)?.length ?? 0, clauses.filter((c) => c.why).length, `whys: ${route}`);
  }
});

test("every clause and error reference carries a server-rendered statement preview", () => {
  let refs = 0;
  for (const path of pages()) {
    for (const [ref, inner] of htmlFor(path).matchAll(/<a\b[^>]*class="(?:clause-ref|error-ref)[^"]*"[^>]*>([\s\S]*?)<\/a>/g)) {
      refs++;
      assert.ok(inner.includes('class="clause-preview"'), `${path}: ${ref.slice(0, 120)}`);
      assert.ok(/\btitle="/.test(ref), `${path}: title fallback`);
    }
  }
  assert.ok(refs > 1000, `${refs} references`);
});

test("every error code span links to its owning clause outside that clause", () => {
  const errors = registeredErrors();
  const failures = [];
  // The search page names example terms; the Markdown pages carry the references.
  for (const path of sitePaths().filter((path) => path.startsWith("/docs/"))) {
    const html = htmlFor(path);
    for (const m of html.matchAll(/<code>([A-Z][A-Za-z0-9]+)<\/code>/g)) {
      const owner = errors.get(m[1])?.clause;
      if (!owner) continue;
      // A title may hold a literal `>`, so the link is read from its start to the span.
      const open = html.lastIndexOf("<a ", m.index);
      const link = html.slice(open, m.index);
      if (link.endsWith('class="error-ref">') && link.startsWith(`<a href="`) && link.includes(`#${owner}"`)) continue;
      const item = html.lastIndexOf('<li class="clause" id="', m.index);
      const inOwner = item >= 0 && html.lastIndexOf("</li>", m.index) < item
        && html.startsWith(`<li class="clause" id="${owner}"`, item);
      const inHeading = /<h[1-6][^>]*>[^<]*$/.test(html.slice(Math.max(0, m.index - 200), m.index));
      if (!inOwner && !inHeading) failures.push(`${path}: ${m[1]}`);
    }
  }
  assert.deepEqual(failures, []);
});

test("the search index holds every clause id and every registered error", () => {
  const dir = "dist/pagefind/fragment";
  const fragments = readdirSync(dir).map((f) => JSON.parse(gunzipSync(readFileSync(join(dir, f))).toString().replace(/^pagefind_dcd/, "")));
  const byTitle = new Map(fragments.map((f) => [f.meta?.title, f]));
  const missing = lock.clauses.filter((c) => !byTitle.get(c.id)?.url.endsWith(`#${c.id}`)).map((c) => c.id);
  assert.deepEqual(missing, []);
  const unindexed = [...registeredErrors()].filter(([name, { clause }]) => !byTitle.get(clause)?.content.includes(name)).map(([name]) => name);
  assert.deepEqual(unindexed, []);
  assert.ok(htmlFor("/docs/search/").includes('src="/pagefind/pagefind-ui.js"'));
  assert.ok(!htmlFor("/docs/search/").includes("data-pagefind-body"), "the search page stays out of its own index");
  assert.ok(!fragments.some((f) => f.url === "/docs/search/"));
  assert.ok(htmlFor("/docs/").includes('href="/docs/search/"'));
});

test("pinned clauses show their pins beside the id", { skip: !lock.pins && "the lock carries no pins" }, () => {
  for (const id of Object.keys(lock.pins ?? {})) {
    const clause = lock.clauses.find((c) => c.id === id);
    if (!clause) continue;
    const html = htmlFor(routeFor(resolve(SPEC_DIR, "..", clause.file)));
    const at = html.indexOf(`<li class="clause" id="${id}">`);
    assert.ok(at >= 0 && html.slice(at, html.indexOf("</li>", at)).includes('class="clause-pins"'), id);
  }
});

test("the glossary renders as a page when spec/glossary.md exists", { skip: !existsSync(join(SPEC_DIR, "glossary.md")) && "spec/glossary.md is absent" }, () => {
  assert.ok(sitePaths().includes("/docs/specs/glossary/"));
  assert.ok(htmlFor("/docs/specs/glossary/").includes('<article class="prose" data-pagefind-body>'));
  assert.ok(htmlFor("/docs/").includes('href="/docs/specs/glossary/"'));
});
