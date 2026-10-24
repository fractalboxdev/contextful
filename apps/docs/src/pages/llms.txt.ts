import type { APIRoute } from "astro";
import { sourceDocuments } from "../lib/corpus.mjs";

export const GET: APIRoute = ({ site }) => new Response([
  "# Contextful", "", "> A local-first context engine for agents and small teams.", "",
  `- [Documentation](${new URL("/docs/", site)})`,
  `- [Full Markdown corpus](${new URL("/llms-full.txt", site)})`, "", "## Specification and guides", "",
  ...sourceDocuments().map(({ route, body }) => `- [${/^# (.+)$/m.exec(body)?.[1].replace(/[`*]/g, "") ?? route}](${new URL(route, site)})`), "",
].join("\n"), { headers: { "Content-Type": "text/plain; charset=utf-8" } });
