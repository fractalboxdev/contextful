import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { prepareConformanceImage } from "../src/prepare-image.ts";

const hash = (value: Uint8Array) => createHash("sha256").update(value).digest("hex");

test("the Container image context uses the release archive's exact binary", () => {
  const root = mkdtempSync(join(tmpdir(), "contextful-image-test-"));
  try {
    const stem = "contextful-full-test-x86_64-unknown-linux-musl";
    mkdirSync(join(root, stem));
    const binary = Buffer.from("exact-release-binary");
    writeFileSync(join(root, stem, "contextful"), binary);
    writeFileSync(join(root, stem, "contextful-conformance"), "suite");
    const archive = join(root, `${stem}.tar.gz`);
    execFileSync("tar", ["-czf", archive, stem], { cwd: root });
    const source = {
      artifact_digest: hash(binary),
      source_commit: "a".repeat(40),
      suite_revision: "suite-v1",
    };
    const release = join(root, "release.json");
    writeFileSync(release, JSON.stringify({
      profile: "contextful-full",
      target: "x86_64-unknown-linux-musl",
      archive: `${stem}.tar.gz`,
      sha256: hash(readFileSync(archive)),
      d1_compatibility: source,
    }));
    const report = join(root, "d1.json");
    writeFileSync(report, JSON.stringify({ ...source, source_dirty: false }));
    const output = join(root, "image");
    prepareConformanceImage(archive, release, report, output);
    assert.deepEqual(readFileSync(join(output, "contextful")), binary);
    assert.deepEqual(JSON.parse(readFileSync(join(output, "source.json"), "utf8")), source);
    assert.throws(() => prepareConformanceImage(archive, release, report, output), /ConformanceImageContextExists/);
    writeFileSync(report, JSON.stringify({ ...source, source_dirty: true }));
    assert.throws(() => prepareConformanceImage(archive, release, report, join(root, "dirty-image")),
      /ConformanceReleaseEvidenceMismatch/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
