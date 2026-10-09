// Builds client/dist/{query,admin}.html: each page is one self-contained document —
// prerendered markup, the hydrating bundle and the compiled stylesheet inline — so the
// server's `default-src 'none'` policy holds with no asset routes.
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { build } from "esbuild";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const client = join(root, "client");
const out = join(client, "dist");
mkdirSync(out, { recursive: true });

const shared = { bundle: true, jsx: "automatic", write: false, logLevel: "warning", define: { "process.env.NODE_ENV": '"production"' } };

const browser = await build({ ...shared, entryPoints: [join(client, "main.tsx")], platform: "browser", format: "iife", target: "es2022", minify: true });
const script = browser.outputFiles[0].text.replace(/<\/script/gi, "<\\/script");

const prerenderFile = join(out, ".prerender.mjs");
await build({ ...shared, write: true, entryPoints: [join(client, "prerender.tsx")], platform: "node", format: "esm", target: "node22", packages: "external", outfile: prerenderFile });
const { render } = await import(pathToFileURL(prerenderFile).href);
rmSync(prerenderFile);

const tailwind = join(root, "node_modules", ".bin", "tailwindcss");
const compiled = execFileSync(tailwind, ["-i", join(client, "styles.css"), "--minify"], { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] });
const font = readFileSync(join(root, "node_modules", "@fontsource-variable", "geist", "files", "geist-latin-wght-normal.woff2")).toString("base64");
const style = `@font-face{font-family:"Geist Variable";font-style:normal;font-display:swap;font-weight:100 900;src:url(data:font/woff2;base64,${font}) format("woff2")}${compiled}`
  .replace(/<\/style/gi, "<\\/style");

const titles = { query: "Query · Contextful", admin: "Admin · Contextful" };
for (const page of ["query", "admin"]) {
  const html = `<!doctype html><html lang="en" class="dark"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">` +
    `<meta name="robots" content="noindex, nofollow"><title>${titles[page]}</title><style>${style}</style></head>` +
    `<body class="min-h-screen"><div id="root" data-page="${page}">${render(page)}</div><script>${script}</script></body></html>`;
  writeFileSync(join(out, `${page}.html`), html);
}
console.log(`client: query.html ${(readFileSync(join(out, "query.html")).length / 1024).toFixed(0)} KiB`);
