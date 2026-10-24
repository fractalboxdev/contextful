import type { APIRoute } from "astro";
import { sourceDocuments } from "../lib/corpus.mjs";

export const GET: APIRoute = ({ site }) => new Response(
  sourceDocuments().map(({ route, body }) => `Source: ${new URL(route, site)}\n\n${body}`).join("\n\n---\n\n"),
  { headers: { "Content-Type": "text/plain; charset=utf-8" } },
);
