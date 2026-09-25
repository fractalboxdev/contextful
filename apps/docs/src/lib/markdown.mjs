// Markdown plugins that turn the spec's plain-Markdown conventions into site links.
import { dirname, resolve } from "node:path";
import { visit, SKIP } from "unist-util-visit";
import { readFileSync } from "node:fs";
import { corpusIndex, routeFor } from "./corpus.mjs";

const CLAUSE_ID = /^[a-z0-9-]+\.[a-z0-9-]+\.[a-z0-9.-]+$/;
// `{{clause.id}}` references and bare record ids (P4, D07, A-store).
const INLINE_REF = /\{\{([a-z0-9-]+\.[a-z0-9-]+\.[a-z0-9.-]+)\}\}|\b(P[1-9]|D\d{2}|A-[a-z]+)\b/g;

const sourcePath = (file) => file.path ?? file.history?.[0];

/** Front-matter `contract` of the source file; guides and cards carry one too, so only `spec/NN-*.md` counts. */
const contractOf = (file, from) => {
  if (!from || !/^\/spec\/[^/]+\/$/.test(routeFor(from) ?? "")) return undefined;
  const fm = file.data?.astro?.frontmatter?.contract;
  if (typeof fm === "string") return fm;
  const src = readFileSync(from, "utf8");
  return /^---\n[\s\S]*?^contract:\s*(\S+)[\s\S]*?\n---/m.exec(src)?.[1];
};

const hp = (className, extra = {}) => ({ hProperties: { className: [className], ...extra } });

/** A link to a clause: its subject in code, its full id and statement in the tooltip. */
const clauseLink = (id, target) => ({
  type: "link",
  url: `${target.route}#${id}`,
  title: `${id} — ${target.statement}`,
  data: hp("clause-ref"),
  children: [{ type: "inlineCode", value: id.split(".").at(-1) }],
});

/**
 * Turns each operation section of a contract file into addressable rules: the lede
 * paragraph gets `lede`, and each item of the clause list becomes `<li id="<clause id>">`
 * with a margin anchor on its subject, the statement, and its Why beneath.
 */
const addressClauses = (tree, contract, owns) => {
  const kids = tree.children;
  for (let i = 0; i < kids.length; i++) {
    const h = kids[i];
    if (h.type !== "heading" || h.depth !== 2) continue;
    const op = h.children.map((c) => c.value ?? "").join("").trim();
    if (!owns.has(op)) continue;
    let j = i + 1;
    if (kids[j]?.type === "paragraph") {
      kids[j].data = { ...kids[j].data, ...hp("lede") };
      j++;
    }
    while (kids[j] && kids[j].type !== "list" && kids[j].type !== "heading") j++;
    const list = kids[j];
    if (list?.type !== "list") continue;
    list.data = { ...list.data, ...hp("clauses") };
    for (const item of list.children) {
      const para = item.children[0];
      const [code, sep] = para?.type === "paragraph" ? para.children : [];
      if (code?.type !== "inlineCode" || sep?.type !== "text" || !sep.value.startsWith(" — ")) continue;
      const id = `${contract}.${op}.${code.value}`;
      const rest = para.children.slice(1);
      rest[0] = { ...sep, value: sep.value.slice(3) };
      // The Why follows a soft break: a trailing emphasis node.
      let why;
      const last = rest.at(-1);
      const beforeLast = rest.at(-2);
      if (last?.type === "emphasis" && beforeLast?.type === "text" && /\n\s*$/.test(beforeLast.value)) {
        why = rest.pop();
        rest[rest.length - 1] = { ...beforeLast, value: beforeLast.value.replace(/\s*\n\s*$/, "") };
      }
      item.data = { ...item.data, ...hp("clause", { id }) };
      para.children = [
        { type: "link", url: `#${id}`, title: id, data: hp("clause-anchor"), children: [code] },
        { type: "clauseStatement", data: { hName: "span", ...hp("clause-statement") }, children: rest },
        ...(why ? [{ type: "clauseWhy", data: { hName: "span", ...hp("clause-why") }, children: why.children }] : []),
      ];
    }
  }
};

/**
 * Rewrites `.md` links to routes, addresses clause items, and links `{{id}}` and record ids.
 */
export function remarkCorpus() {
  return (tree, file) => {
    const { clauses, operations, records } = corpusIndex();
    const from = sourcePath(file);
    const self = from ? routeFor(from) : undefined;

    visit(tree, "link", (node) => {
      const m = /^([^:#?]+\.md)(#.*)?$/.exec(node.url);
      if (!m || !from) return;
      const route = routeFor(resolve(dirname(from), m[1]));
      if (route) node.url = route + (m[2] ?? "");
    });

    const contract = contractOf(file, from);
    if (contract) {
      const owns = new Set(
        [...operations.keys()].filter((k) => k.startsWith(`${contract}.`)).map((k) => k.slice(contract.length + 1)),
      );
      addressClauses(tree, contract, owns);
    }

    // `store.fold.partial-snapshot` links to its row, `store.fold` to its section.
    visit(tree, "inlineCode", (node, index, parent) => {
      if (!parent || parent.type === "link" || parent.type === "heading") return;
      const target = clauses.get(node.value);
      if (target) {
        parent.children[index] = { ...clauseLink(node.value, target), children: [node] };
        return;
      }
      const url = operations.get(node.value);
      if (url) parent.children[index] = { type: "link", url, data: hp("clause-ref"), children: [node] };
    });

    visit(tree, "text", (node, index, parent) => {
      if (!parent || parent.type === "link" || parent.type === "heading") return;
      const out = [];
      let last = 0;
      for (const m of node.value.matchAll(INLINE_REF)) {
        const [raw, clause, record] = m;
        const target = clause ? clauses.get(clause) : records.get(record);
        if (!target || (record && target === self)) continue;
        if (m.index > last) out.push({ type: "text", value: node.value.slice(last, m.index) });
        out.push(
          clause
            ? clauseLink(clause, target)
            : { type: "link", url: target, data: hp("record-ref"), children: [{ type: "text", value: record }] },
        );
        last = m.index + raw.length;
      }
      if (!out.length) return;
      if (last < node.value.length) out.push({ type: "text", value: node.value.slice(last) });
      parent.children.splice(index, 1, ...out);
      return [SKIP, index + out.length];
    });
  };
}

const hasClass = (node, name) => {
  const c = node.properties?.className;
  return Array.isArray(c) ? c.includes(name) : typeof c === "string" && c.split(/\s+/).includes(name);
};

/** `<file>:<line>` of each mermaid fence Merlion did not draw in this build. */
export const unrendered = [];

/** Fails the build when any mermaid fence reached the page as code. */
export const diagramGate = {
  name: "diagram-gate",
  hooks: {
    "astro:build:done": () => {
      if (unrendered.length) {
        throw new Error(`Merlion did not render ${unrendered.length} mermaid fence(s): ${unrendered.join(", ")}`);
      }
    },
  },
};

/**
 * Wraps each table so a wide one scrolls horizontally instead of the page. A mermaid fence
 * Merlion left as code is recorded in `unrendered`; the site ships no client-side
 * renderer, so the build fails on any.
 */
export function rehypeCorpus() {
  return (tree, file) => {
    visit(tree, "element", (node, index, parent) => {
      if (!parent || index === undefined) return;
      if (node.tagName === "table") {
        parent.children[index] = { type: "element", tagName: "div", properties: { className: ["table-wrap"] }, children: [node] };
        return SKIP;
      }
      if (node.tagName === "code" && hasClass(node, "language-mermaid")) {
        unrendered.push(`${sourcePath(file) ?? "?"}:${node.position?.start.line ?? 0}`);
      }
    });
  };
}
