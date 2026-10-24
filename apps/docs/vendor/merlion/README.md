# Merlion v0.2.0

The four package archives contain the official [v0.2.0 source](https://github.com/fractalboxdev/merlion/tree/f5b222e8d22b90a2df862ed6a92c70222f212fcd) under the `@fractalboxdev` scope. Vendoring keeps installation independent of GitHub Packages authentication.

`merlion-wasm` contains the [released WebAssembly binary](https://github.com/fractalboxdev/merlion/releases/tag/v0.2.0), verified against the release's `SHA256SUMS`:

```text
a377551b962f133a29e67755056f17630e3c7303a29ef27b73c53704dc76ad67  merlion.wasm
```

Each archive contains the package's published file list. Repack from the tagged source with `pnpm pack`; `merlion-rehype` uses `npm pack --ignore-scripts` because its development dependencies use the workspace protocol. Copy the verified release binary into `packages/merlion-wasm/merlion.wasm` before packing that package.
