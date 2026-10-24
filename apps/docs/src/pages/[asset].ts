import type { APIRoute } from "astro";
import { legacyRedirects } from "../lib/corpus.mjs";

export const getStaticPaths = () => [{ params: { asset: "_redirects" } }];

export const GET: APIRoute = ({ site }) => new Response([
  ...Object.entries(legacyRedirects()).map(([from, to]) => `${from} ${new URL(to, site)} 301`),
  "",
].join("\n"));
