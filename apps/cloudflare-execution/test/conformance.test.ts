import assert from "node:assert/strict";
import { test } from "node:test";
import { completeConformanceReport } from "../src/conformance.ts";

const source = {
  artifact_digest: "a".repeat(64),
  source_commit: "b".repeat(40),
  suite_revision: "suite-v1",
};

const report = {
  artifact_digest: source.artifact_digest,
  suite_revision: source.suite_revision,
  storage_profile: "cloudflare-d1",
  feature_profile: "contextful-full",
  scenarios: [{ required: true, outcome: "passed" }],
};

test("the D1 report binds successful scenarios to the release binary and source", () => {
  assert.deepEqual(completeConformanceReport(report, source), {
    ...report,
    source_commit: source.source_commit,
    source_dirty: false,
    binding_origin: "cloudflare-worker",
    compatible: true,
  });
});

test("the D1 report refuses a different artifact, a skip, or an unsupported scenario", () => {
  assert.throws(() => completeConformanceReport({ ...report, artifact_digest: "c".repeat(64) }, source),
    /ConformanceReleaseEvidenceMismatch/);
  for (const outcome of ["skipped", "unsupported", "failed"]) {
    assert.equal(completeConformanceReport({ ...report, scenarios: [{ required: true, outcome }] }, source).compatible, false);
  }
});
