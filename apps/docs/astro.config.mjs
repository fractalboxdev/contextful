import { defineConfig } from "astro/config";
import rehypeMerlion from "@fractalbox/merlion-rehype";
import { diagramGate, remarkCorpus, rehypeCorpus } from "./src/lib/markdown.mjs";

// The dev server answers localhost alone. DOCS_ALLOWED_HOSTS names the exact
// hosts, comma-separated, that may reach it: it serves the repository root, and a
// suffix such as `.ts.net` spans tailnets whose 100.x addresses overlap this one.
const allowedHosts = (process.env.DOCS_ALLOWED_HOSTS ?? "").split(",").filter(Boolean);

export default defineConfig({
  trailingSlash: "always",
  integrations: [diagramGate],
  markdown: {
    syntaxHighlight: { type: "shiki", excludeLangs: ["mermaid"] },
    shikiConfig: { themes: { light: "github-light", dark: "github-dark" } },
    remarkPlugins: [remarkCorpus],
    // Merlion draws every mermaid fence to inline SVG at build time; diagramGate fails the
    // build on a fence it leaves as code.
    rehypePlugins: [
      [rehypeMerlion, { width: 720, source: "none", fontCss: true, cacheDir: ".merlion" }],
      rehypeCorpus,
    ],
  },
  vite: {
    server: { allowedHosts, fs: { allow: ["../.."] } },
  },
});
