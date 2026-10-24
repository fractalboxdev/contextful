import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

test("the canonical deployment needs only its own hostname's zone", () => {
  const { config, error } = ts.parseConfigFileTextToJson("wrangler.jsonc", readFileSync("wrangler.jsonc", "utf8"));
  assert.equal(error, undefined);
  assert.deepEqual(config.routes.map((route) => route.pattern), ["contextful.fractalbox.dev"]);
});

test("the legacy hostname has an independent deployment without site assets", () => {
  const { config, error } = ts.parseConfigFileTextToJson("wrangler.legacy.jsonc", readFileSync("wrangler.legacy.jsonc", "utf8"));
  assert.equal(error, undefined);
  assert.deepEqual(config.routes.map((route) => route.pattern), ["spec.contextful.work"]);
  assert.equal(config.main, "src/legacy.ts");
  assert.equal(config.assets, undefined);
});
