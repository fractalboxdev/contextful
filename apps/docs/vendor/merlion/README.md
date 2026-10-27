# Merlion

The four package archives hold Merlion under the `@fractalboxdev` scope. Vendoring keeps installation independent of GitHub Packages authentication.

| Archive | Version | Source |
| --- | --- | --- |
| `merlion-themes` | 0.2.0 | the official [v0.2.0 source](https://github.com/fractalboxdev/merlion/tree/f5b222e8d22b90a2df862ed6a92c70222f212fcd) |
| `merlion-view` | 0.2.1-pre.1 | unreleased source after v0.2.0: the inspector panel (`interact-details.js`) and its event containment |
| `merlion-wasm` | 0.2.1-pre.1 | a WebAssembly binary built from the same unreleased source |
| `merlion-rehype` | 0.2.1-pre.1 | the v0.2.0 source, its `merlion-wasm` peer moved to `0.2.1-pre.1` |

An archive whose content changes takes a new version, so a pnpm store or CI cache holding an older archive under the same version cannot satisfy the lockfile's integrity hash. The `-pre.N` versions stay clear of a future upstream `0.2.1` release; replace them with that release's archives when it ships.

`merlion-wasm` 0.2.1-pre.1 holds this binary:

```text
f5219eb57a4065cebb87bea2b34dc7fcd4422d608a2234d5e2af815e55a4469b  merlion.wasm
```

Each archive contains the package's published file list. Repack a released version from its tagged source with `pnpm pack`; `merlion-rehype` uses `npm pack --ignore-scripts` because its development dependencies use the workspace protocol. Copy the release's verified binary into `packages/merlion-wasm/merlion.wasm` before packing that package.
