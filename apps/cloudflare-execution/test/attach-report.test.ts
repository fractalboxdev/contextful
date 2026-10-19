import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { attachConformanceReport } from "../src/attach-report.ts";

const hash = (value: Uint8Array) => createHash("sha256").update(value).digest("hex");

test("a provider pass enters the archive and companion release evidence", () => {
  const root = mkdtempSync(join(tmpdir(), "contextful-attach-test-"));
  try {
    const stem = "contextful-full-test-x86_64-unknown-linux-musl";
    const archive = `${stem}.tar.gz`;
    mkdirSync(join(root, stem));
    const binary = Buffer.from("exact-release-binary");
    writeFileSync(join(root, stem, "contextful"), binary);
    writeFileSync(join(root, stem, "contextful-conformance"), "suite");
    const baseline = {
      artifact_digest: hash(binary),
      source_commit: "a".repeat(40),
      source_dirty: false,
      suite_revision: "suite-v1",
      storage_profile: "cloudflare-d1",
      feature_profile: "contextful-full",
      binding_origin: "unverified",
      compatible: false,
      scenarios: [{ id: "replacement", required: true, outcome: "failed" }],
    };
    writeFileSync(join(root, stem, "cloudflare-d1.compatibility.json"), JSON.stringify(baseline));
    execFileSync("tar", ["-czf", archive, stem], { cwd: root });
    writeFileSync(join(root, `${stem}.cloudflare-d1.compatibility.json`), JSON.stringify(baseline));
    const metadataPath = join(root, `${stem}.release.json`);
    writeFileSync(metadataPath, JSON.stringify({
      profile: "contextful-full",
      archive,
      sha256: hash(readFileSync(join(root, archive))),
      d1_compatibility: {
        report: `${stem}.cloudflare-d1.compatibility.json`,
        artifact_digest: baseline.artifact_digest,
        source_commit: baseline.source_commit,
        suite_revision: baseline.suite_revision,
        storage_profile: "cloudflare-d1",
        binding_origin: "absent",
        compatible: false,
        scenarios: baseline.scenarios,
      },
    }));
    const provider = { ...baseline, binding_origin: "cloudflare-worker", compatible: true,
      suite_exit_code: 0, scenarios: [{ id: "replacement", required: true, outcome: "passed" }] };
    const providerPath = join(root, "provider.json");
    writeFileSync(providerPath, JSON.stringify(provider));
    const output = join(root, "attached");
    attachConformanceReport(metadataPath, providerPath, output);
    const attached = JSON.parse(readFileSync(join(output, `${stem}.release.json`), "utf8")).d1_compatibility;
    assert.equal(attached.compatible, true);
    assert.equal(attached.binding_origin, "cloudflare-worker");
    assert.deepEqual(attached.scenarios, provider.scenarios);
    assert.equal(JSON.parse(readFileSync(join(output, `${stem}.cloudflare-d1.compatibility.json`), "utf8")).binding_origin, "cloudflare-worker");
    const embedded = execFileSync("tar", ["-xOzf", join(output, archive), `${stem}/cloudflare-d1.compatibility.json`]);
    assert.equal(JSON.parse(embedded.toString()).compatible, true);
    assert.equal(hash(readFileSync(join(output, archive))),
      JSON.parse(readFileSync(join(output, `${stem}.release.json`), "utf8")).sha256);
    const skippedPath = join(root, "skipped.json");
    writeFileSync(skippedPath, JSON.stringify({ ...provider, scenarios: [{ id: "replacement", required: true, outcome: "skipped" }] }));
    assert.throws(() => attachConformanceReport(metadataPath, skippedPath, join(root, "skipped-output")),
      /ConformanceReleaseEvidenceMismatch/);
    writeFileSync(join(root, `${stem}.cloudflare-d1.compatibility.json`), JSON.stringify({ ...baseline, source_dirty: true }));
    assert.throws(() => attachConformanceReport(metadataPath, providerPath, join(root, "dirty-output")),
      /ConformanceReleaseEvidenceMismatch/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
