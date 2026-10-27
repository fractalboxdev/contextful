import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { parse } from "smol-toml";
import { release } from "../src/lib/corpus.mjs";

test("the site names the release the workspace version targets", () => {
  const cargo = parse(readFileSync(resolve(process.cwd(), "../../Cargo.toml"), "utf8"));
  const version = cargo.workspace.package.version;
  assert.deepEqual(release(), { tag: `v${version}`, href: `https://github.com/fractalboxdev/contextful/releases/tag/v${version}` });
});
