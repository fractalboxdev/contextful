import type { APIRoute } from "astro";
import { sitePaths } from "../lib/corpus.mjs";

export const GET: APIRoute = ({ site }) => new Response(
  `<?xml version="1.0" encoding="UTF-8"?><urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">${sitePaths().map((path) => `<url><loc>${new URL(path, site)}</loc></url>`).join("")}</urlset>`,
  { headers: { "Content-Type": "application/xml" } },
);
