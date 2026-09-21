import { defineConfig } from "astro/config";
import rehypeMermaid from "rehype-mermaid";
import { remarkCorpus, rehypeCorpus } from "./src/lib/markdown.mjs";

// The dev server answers localhost alone. DOCS_ALLOWED_HOSTS names the exact
// hosts, comma-separated, that may reach it: it serves the repository root, and a
// suffix such as `.ts.net` spans tailnets whose 100.x addresses overlap this one.
const allowedHosts = (process.env.DOCS_ALLOWED_HOSTS ?? "").split(",").filter(Boolean);

export default defineConfig({
  trailingSlash: "always",
  markdown: {
    syntaxHighlight: { type: "shiki", excludeLangs: ["mermaid"] },
    shikiConfig: { themes: { light: "github-light", dark: "github-dark" } },
    remarkPlugins: [remarkCorpus],
    // Diagrams render to SVG at build time, a light and a dark variant each; a diagram
    // that fails to parse fails the build.
    rehypePlugins: [[rehypeMermaid, { strategy: "img-svg", dark: true }], rehypeCorpus],
  },
  vite: {
    server: { allowedHosts, fs: { allow: ["../.."] } },
  },
});
