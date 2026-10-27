import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { emptyFiles, writeComplete } from "../src/lib/search.mjs";

const tree = () => {
  const dir = mkdtempSync(join(tmpdir(), "pagefind-"));
  mkdirSync(join(dir, "fragment"));
  writeFileSync(join(dir, "pagefind.js"), "x");
  return dir;
};

test("emptyFiles names every zero-byte file under the index directory", () => {
  const dir = tree();
  writeFileSync(join(dir, "fragment", "en_a.pf_fragment"), "");
  writeFileSync(join(dir, "fragment", "en_b.pf_fragment"), "gz");
  assert.deepEqual(emptyFiles(dir), [join("fragment", "en_a.pf_fragment")]);
});

test("writeComplete rewrites the index until no file is empty, and refuses after three writes", async () => {
  const dir = tree();
  let writes = 0;
  const healing = {
    writeFiles: async () => {
      writes += 1;
      writeFileSync(join(dir, "fragment", "en_a.pf_fragment"), writes === 1 ? "" : "gz");
      return { errors: [] };
    },
  };
  await writeComplete(healing, dir);
  assert.equal(writes, 2);

  const broken = { writeFiles: async () => (writeFileSync(join(dir, "fragment", "en_a.pf_fragment"), ""), { errors: [] }) };
  await assert.rejects(writeComplete(broken, dir), /Pagefind write: 1 empty file\(s\) after 3 writes, first fragment\/en_a\.pf_fragment/);
});
