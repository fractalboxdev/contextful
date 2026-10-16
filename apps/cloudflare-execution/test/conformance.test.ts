import assert from "node:assert/strict";
import { test } from "node:test";
import { admitConformanceRun, completeConformanceReport } from "../src/conformance.ts";

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

function container(live: { suite: boolean }) {
  const calls: string[][] = [];
  const exec = async (argv: string[]) => {
    calls.push(argv);
    if (argv[0] === "/usr/bin/pgrep") return { exitCode: live.suite ? 0 : 1 };
    return { exitCode: 0 };
  };
  return { calls, exec };
}

test("a suite starts only after the artifact processes an earlier run left behind end", async () => {
  const { calls, exec } = container({ suite: false });
  await admitConformanceRun(exec);
  assert.deepEqual(calls, [
    ["/usr/bin/pgrep", "-f", "/usr/local/bin/contextful-conformance"],
    ["/usr/bin/pkill", "-KILL", "-f", "/usr/local/bin/contextful"],
  ]);
});

test("a suite refuses while another suite process still drives the bindings", async () => {
  const { calls, exec } = container({ suite: true });
  await assert.rejects(admitConformanceRun(exec), /ConformanceAlreadyRunning/);
  assert.equal(calls.length, 1);
});

test("a container that cannot list its processes admits no suite", async () => {
  await assert.rejects(admitConformanceRun(async () => ({ exitCode: 127 })), /ConformanceContainerUnavailable/);
});
