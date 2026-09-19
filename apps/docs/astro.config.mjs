import { defineConfig } from "astro/config";
import { remarkCorpus, rehypeCorpus } from "./src/lib/markdown.mjs";

export default defineConfig({
  trailingSlash: "always",
  markdown: {
    syntaxHighlight: { type: "shiki", excludeLangs: ["mermaid"] },
    shikiConfig: { themes: { light: "github-light", dark: "github-dark" } },
    remarkPlugins: [remarkCorpus],
    rehypePlugins: [rehypeCorpus],
  },
  vite: {
    server: { allowedHosts: [".ts.net"], fs: { allow: ["../.."] } },
    // mermaid is loaded on demand, only on pages that carry a diagram.
    build: { chunkSizeWarningLimit: 3000 },
  },
});
