# Contextful website

`pnpm typecheck` generates Worker types and checks Astro. `pnpm build` renders the site and runs its regression tests. `pnpm deploy` publishes the site to `contextful.fractalbox.dev`.

The production account contains the `fractalbox.dev` zone. Set `CLOUDFLARE_ACCOUNT_ID` to that account's ID when a cached Wrangler account differs from the current login.

The optional `pnpm exec wrangler deploy --config wrangler.legacy.jsonc` publishes the former-hostname redirect in the account containing `contextful.work`. The production deployment has no dependency on that zone. The canonical site redirects former content paths through its generated `_redirects` file.
