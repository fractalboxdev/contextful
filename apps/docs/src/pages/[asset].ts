import type { APIRoute } from "astro";
import { legacyRedirects } from "../lib/corpus.mjs";

export const getStaticPaths = () => [{ params: { asset: "_redirects" } }];

export const GET: APIRoute = ({ site }) => new Response([
  `https://spec.contextful.work/ ${new URL("/docs/specs/", site)} 301`,
  ...Object.entries(legacyRedirects()).flatMap(([from, to]) => [
    `https://spec.contextful.work${from} ${new URL(to, site)} 301`,
    `${from} ${new URL(to, site)} 301`,
  ]),
  `https://spec.contextful.work/* ${new URL("/:splat", site)} 301`,
  "",
].join("\n"));
