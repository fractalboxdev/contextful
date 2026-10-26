// Markdown plugins that turn the spec's plain-Markdown conventions into site links.
import { dirname, resolve } from "node:path";
import { visit, SKIP } from "unist-util-visit";
import { existsSync, readFileSync } from "node:fs";
import { ADR_DIR, corpusIndex, recordId, routeFor, SPEC_DIR } from "./corpus.mjs";

// `{{contract.operation.subject}}` and `{{contract.operation}}` pointers, and bare record
// ids (P4, D07, A-store), a contract ADR optionally naming its section (A-store#lease).
const INLINE_REF = /\{\{([a-z0-9-]+\.[a-z0-9-]+(?:\.[a-z0-9.-]+)?)\}\}|\b(P[1-9]|D\d{2}|A-[a-z]+)(?:#([a-z0-9-]+))?\b/g;

const sourcePath = (file) => file.path ?? file.history?.[0];

/** The topology deployment section includes the generated target tables and runtime diagrams. */
export function remarkTopologyTargets() {
  const processor = this;
  return (tree, file) => {
    if (sourcePath(file) !== resolve(SPEC_DIR, "01-topology.md")) return;
    const targets = processor.parse(readFileSync(resolve(SPEC_DIR, "targets.md"), "utf8"));
    // The generated file's provenance stays in its source; the site carries one page title.
    const providers = targets.children.findIndex((node) => node.type === "heading" && node.depth === 2);
    if (providers < 0) throw new Error("Deployment targets contain no provider sections");
    const content = targets.children.slice(providers);
    for (const node of content) {
      if (node.type === "heading") node.depth += 1;
    }
    const at = tree.children.findIndex((node) => node.type === "heading" && node.depth === 2 && node.children[0]?.value === "publish-hostname");
    if (at < 0) throw new Error("Topology contains no publish-hostname section");
    tree.children.splice(at, 0,
      { type: "heading", depth: 2, children: [{ type: "text", value: "Deployment targets" }] },
      ...content,
    );
  };
}

/** Front-matter `contract` of the source file; guides and cards carry one too, so only `spec/NN-*.md` counts. */
const contractOf = (file, from) => {
  if (!from || dirname(from) !== SPEC_DIR || !existsSync(from)) return undefined;
  const fm = file.data?.astro?.frontmatter?.contract;
  if (typeof fm === "string") return fm;
  const src = readFileSync(from, "utf8");
  return /^---\n[\s\S]*?^contract:\s*(\S+)[\s\S]*?\n---/m.exec(src)?.[1];
};

const hp = (className, extra = {}) => ({ hProperties: { className: [className].flat(), ...extra } });
const el = (type, hName, className, children, extra) => ({ type, data: { hName, ...hp(className, extra) }, children });
const text = (value) => ({ type: "text", value });
const plain = (node) => node.value ?? (node.children ?? []).map(plain).join("");

/**
 * The statement a reference resolves to, rendered beside it and shown on hover and focus.
 * The link's `title` carries the same text to assistive technology, so the preview is hidden
 * from it, and search indexes the statement at its clause rather than at every reference.
 */
const preview = (label, statement) =>
  el("clausePreview", "span", "clause-preview", [el("clausePreviewId", "span", "clause-preview-id", [text(label)]), text(statement)], {
    ariaHidden: "true",
    dataPagefindIgnore: true,
  });

/** A link to a clause: its subject (or `shown`) in code, its full id and statement in the tooltip and preview. */
const clauseLink = (id, target, shown, className = "clause-ref") => ({
  type: "link",
  url: `${target.route}#${id}`,
  title: `${id} — ${target.statement}`,
  data: hp(className),
  children: [shown ?? { type: "inlineCode", value: id.split(".").at(-1) }, preview(id, target.statement)],
});

/** A link to an operation section: its name in code, its lede in the tooltip and preview. */
const operationLink = (key, target, shown) => ({
  type: "link",
  url: target.url,
  title: target.lede ? `${key} — ${target.lede}` : key,
  data: hp(["clause-ref", "operation-ref"]),
  children: [shown ?? { type: "inlineCode", value: key.split(".").at(-1) }, preview(key, target.lede)],
});

// Text that is already a link, a heading, or a preview inside a link takes no further links.
const UNLINKED = new Set(["link", "heading", "clausePreview", "clausePreviewId"]);

const slug = (s) => s.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");

/** Items of a clause list open with `` `<subject>` — ``. */
const isClauseList = (node) => {
  const [code, sep] = node?.type === "list" ? node.children[0]?.children[0]?.children ?? [] : [];
  return code?.type === "inlineCode" && sep?.type === "text" && sep.value.startsWith(" — ");
};

/**
 * Each item of a clause list becomes `<li id="<clause id>">`: a margin holding the anchor
 * on its subject and the clause's pins, the statement, and its Why folded beneath.
 */
const addressList = (list, contract, op, { clauses, errors }) => {
  list.data = { ...list.data, ...hp("clauses", { dataPagefindIgnore: true }) };
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
    // A clause's own error stays plain inside it; everywhere else the error links here.
    visit({ type: "root", children: rest }, "inlineCode", (node) => {
      if (errors.get(node.value)?.clause === id) node.data = { ...node.data, ownClause: true };
    });
    const pins = clauses.get(id)?.pins ?? [];
    item.data = { ...item.data, ...hp("clause", { id }) };
    para.children = [
      el("clauseMargin", "span", "clause-margin", [
        { type: "link", url: `#${id}`, title: id, data: hp("clause-anchor"), children: [code] },
        ...(pins.length
          ? [el("clausePins", "span", "clause-pins", pins.map((pin) =>
            pin.href
              ? { type: "link", url: pin.href, title: pin.label, data: hp(["clause-pin", `pin-${pin.kind}`]), children: [text(pin.kind)] }
              : el("clausePin", "span", ["clause-pin", `pin-${pin.kind}`], [text(pin.kind)], { title: pin.label })))]
          : []),
      ]),
      el("clauseStatement", "span", "clause-statement", rest),
    ];
    if (why) {
      item.children = [para, el("clauseWhy", "details", "clause-why", [
        el("clauseWhySummary", "summary", "clause-why-label", [text("Why")]),
        el("clauseWhyBody", "span", "clause-why-body", why.children),
      ])];
    }
  }
};

/**
 * Turns each operation section of a contract file into addressable rules: the lede
 * paragraph gets `lede`, and each clause list under the operation or one of its
 * `### <group>` headings is addressed by `addressList`. A group does not enter the id.
 */
const addressClauses = (tree, contract, owns, index) => {
  const kids = tree.children;
  for (let i = 0; i < kids.length; i++) {
    const h = kids[i];
    if (h.type !== "heading" || h.depth !== 2) continue;
    const op = plain(h).trim();
    if (!owns.has(op)) continue;
    let j = i + 1;
    if (kids[j]?.type === "paragraph") {
      kids[j].data = { ...kids[j].data, ...hp("lede") };
      j++;
    }
    while (kids[j] && kids[j].type !== "list" && kids[j].type !== "heading") j++;
    if (isClauseList(kids[j])) addressList(kids[j], contract, op, index);
    for (let k = i + 1; k < kids.length && !(kids[k].type === "heading" && kids[k].depth <= 2); k++) {
      const g = kids[k];
      if (g.type !== "heading" || g.depth !== 3) continue;
      const lede = kids[k + 1]?.type === "paragraph" ? kids[k + 1] : undefined;
      const list = kids[k + (lede ? 2 : 1)];
      if (!isClauseList(list)) continue;
      // The group's anchor carries its operation, so it never takes an operation's own slug.
      g.data = { ...g.data, ...hp("clause-group", { id: `${op}--${slug(plain(g))}` }) };
      if (lede) lede.data = { ...lede.data, ...hp(["lede", "group-lede"]) };
      addressList(list, contract, op, index);
    }
  }
};

/**
 * An ADR section heading `<slug> — <decision>` takes `<slug>` as its anchor, the target of
 * `A-<contract>#<slug>`, and is followed by links to the clauses the lock records citing it.
 */
const addressDecisions = (tree, record, { clauses, sections }) => {
  const kids = tree.children;
  for (let i = 0; i < kids.length; i++) {
    const h = kids[i];
    if (h.type !== "heading" || h.depth !== 2) continue;
    const m = /^([a-z0-9-]+) — \S/.exec(plain(h));
    if (!m) continue;
    h.data = { ...h.data, hProperties: { ...h.data?.hProperties, id: m[1] } };
    const citing = (sections.get(`${record}#${m[1]}`) ?? []).filter((id) => clauses.has(id));
    if (!citing.length) continue;
    kids.splice(i + 1, 0, el("citedBy", "p", "cited-by", [
      el("citedByLabel", "span", "cited-by-label", [text("Cited by")]),
      ...citing.map((id) => clauseLink(id, clauses.get(id), { type: "inlineCode", value: id })),
    ]));
    i++;
  }
};

/**
 * Rewrites `.md` links to routes, addresses clause items, and links `{{id}}` pointers,
 * record ids and registered error identifiers. `lock` replaces the lock file in tests.
 */
export function remarkCorpus({ lock } = {}) {
  return (tree, file) => {
    const index = corpusIndex(lock);
    const { clauses, operations, records, errors } = index;
    const from = sourcePath(file);
    const self = from ? routeFor(from) : undefined;

    visit(tree, "link", (node) => {
      const m = /^([^:#?]+\.md)(#.*)?$/.exec(node.url);
      if (!m || !from) return;
      const route = routeFor(resolve(dirname(from), m[1]));
      if (route) node.url = (m[2] ? route.split("#")[0] : route) + (m[2] ?? "");
    });

    const contract = contractOf(file, from);
    if (contract) {
      const owns = new Set(
        [...operations.keys()].filter((k) => k.startsWith(`${contract}.`)).map((k) => k.slice(contract.length + 1)),
      );
      addressClauses(tree, contract, owns, index);
    }
    if (from && dirname(from) === ADR_DIR) addressDecisions(tree, recordId(from), index);

    // `store.fold.partial-snapshot` links to its row, `store.fold` to its section, and
    // `StorePartialSnapshot` to the clause that owns it.
    visit(tree, "inlineCode", (node, index, parent) => {
      if (!parent || parent.type === "link" || parent.type === "heading") return;
      const target = clauses.get(node.value);
      if (target) {
        parent.children[index] = clauseLink(node.value, target, node);
        return;
      }
      const op = operations.get(node.value);
      if (op) {
        parent.children[index] = operationLink(node.value, op, node);
        return;
      }
      const owner = errors.get(node.value)?.clause;
      if (owner && clauses.has(owner) && !node.data?.ownClause) {
        parent.children[index] = clauseLink(owner, clauses.get(owner), node, "error-ref");
      }
    });

    visit(tree, "text", (node, index, parent) => {
      if (!parent || UNLINKED.has(parent.type)) return;
      const out = [];
      let last = 0;
      for (const m of node.value.matchAll(INLINE_REF)) {
        const [raw, pointer, record, section] = m;
        let link;
        if (pointer) {
          const clause = clauses.get(pointer);
          const op = operations.get(pointer);
          link = clause ? clauseLink(pointer, clause) : op ? operationLink(pointer, op) : undefined;
        } else if (records.has(record)) {
          const route = records.get(record);
          const url = section ? `${route === self ? "" : route}#${section}` : route;
          if (route !== self || section) {
            link = { type: "link", url, data: hp("record-ref"), children: [text(raw)] };
          }
        }
        if (!link) continue;
        if (m.index > last) out.push(text(node.value.slice(last, m.index)));
        out.push(link);
        last = m.index + raw.length;
      }
      if (!out.length) return;
      if (last < node.value.length) out.push(text(node.value.slice(last)));
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
