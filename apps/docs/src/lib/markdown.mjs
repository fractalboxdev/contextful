// Markdown plugins that turn the spec's plain-Markdown conventions into site links.
import { dirname, resolve } from "node:path";
import { visit, SKIP } from "unist-util-visit";
import { corpusIndex, routeFor } from "./corpus.mjs";

const CLAUSE_ID = /^[a-z0-9-]+\.[a-z0-9-]+\.[a-z0-9.-]+$/;
// `{{clause.id}}` references and bare record ids (P4, D07, A-store).
const INLINE_REF = /\{\{([a-z0-9-]+\.[a-z0-9-]+\.[a-z0-9.-]+)\}\}|\b(P[1-9]|D\d{2}|A-[a-z]+)\b/g;

const sourcePath = (file) => file.path ?? file.history?.[0];

/** Rewrites `.md` links to routes and links `{{id}}` and record ids. */
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

    // A clause row: its first cell is exactly the clause id in backticks.
    visit(tree, "tableRow", (row) => {
      const cell = row.children[0];
      const code = cell?.children.length === 1 ? cell.children[0] : undefined;
      if (code?.type !== "inlineCode" || !CLAUSE_ID.test(code.value)) return;
      row.data = { ...row.data, hProperties: { ...row.data?.hProperties, id: code.value } };
      cell.children = [
        { type: "link", url: `#${code.value}`, data: { hProperties: { className: ["clause-anchor"] } }, children: [code] },
      ];
    });

    // `store.fold.partial-snapshot` links to its row, `store.fold` to its section.
    visit(tree, "inlineCode", (node, index, parent) => {
      if (!parent || parent.type === "link" || parent.type === "heading") return;
      const clauseRoute = clauses.get(node.value);
      const url = clauseRoute ? `${clauseRoute}#${node.value}` : operations.get(node.value);
      if (!url) return;
      parent.children[index] = { type: "link", url, data: { hProperties: { className: ["clause-ref"] } }, children: [node] };
    });

    visit(tree, "text", (node, index, parent) => {
      if (!parent || parent.type === "link" || parent.type === "heading") return;
      const out = [];
      let last = 0;
      for (const m of node.value.matchAll(INLINE_REF)) {
        const [raw, clause, record] = m;
        const route = clause ? clauses.get(clause) : records.get(record);
        if (!route || (record && route === self)) continue;
        if (m.index > last) out.push({ type: "text", value: node.value.slice(last, m.index) });
        out.push(
          clause
            ? { type: "link", url: `${route}#${clause}`, data: { hProperties: { className: ["clause-ref"] } }, children: [{ type: "inlineCode", value: clause }] }
            : { type: "link", url: route, data: { hProperties: { className: ["record-ref"] } }, children: [{ type: "text", value: record }] },
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

const isDiagram = (node) =>
  (node.tagName === "img" && String(node.properties?.id ?? "").startsWith("mermaid-")) ||
  (node.tagName === "picture" && node.children.some((c) => c.type === "element" && isDiagram(c)));

/**
 * Wraps each table so a wide one scrolls horizontally instead of the page, and each
 * rendered diagram in a button that opens it full size.
 */
export function rehypeCorpus() {
  return (tree) => {
    visit(tree, "element", (node, index, parent) => {
      if (!parent || index === undefined) return;
      if (node.tagName === "table") {
        parent.children[index] = { type: "element", tagName: "div", properties: { className: ["table-wrap"] }, children: [node] };
        return SKIP;
      }
      if (!isDiagram(node)) return;
      const button = {
        type: "element",
        tagName: "button",
        properties: { type: "button", className: ["diagram-open"], ariaLabel: "Enlarge diagram", title: "Enlarge diagram" },
        children: [node],
      };
      parent.children[index] = { type: "element", tagName: "figure", properties: { className: ["diagram"] }, children: [button] };
      return SKIP;
    });
  };
}
