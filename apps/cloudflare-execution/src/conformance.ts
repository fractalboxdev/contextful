type Scenario = { required: boolean; outcome: string };

type SuiteReport = {
  artifact_digest: string;
  suite_revision: string;
  storage_profile: string;
  feature_profile: string | null;
  scenarios: Scenario[];
  [key: string]: unknown;
};

export type ReleaseProvenance = {
  artifact_digest: string;
  source_commit: string;
  suite_revision: string;
};

/** A D1 verdict names the exact release binary, suite and clean source revision. */
export function completeConformanceReport(report: SuiteReport, source: ReleaseProvenance) {
  if (!/^[a-f0-9]{64}$/.test(source.artifact_digest) ||
      !/^[a-f0-9]{40}$/.test(source.source_commit) ||
      report.artifact_digest !== source.artifact_digest ||
      report.suite_revision !== source.suite_revision) {
    throw new Error("ConformanceReleaseEvidenceMismatch");
  }
  const compatible = report.storage_profile === "cloudflare-d1" &&
    report.feature_profile === "contextful-full" &&
    report.scenarios.length > 0 &&
    report.scenarios.some((scenario) => scenario.required) &&
    report.scenarios.every((scenario) => scenario.outcome === "passed");
  return {
    ...report,
    source_commit: source.source_commit,
    source_dirty: false,
    binding_origin: "cloudflare-worker",
    compatible,
  };
}

/** Runs one command in the Container and answers its exit code. */
export type ContainerExec = (argv: string[]) => Promise<{ exitCode: number }>;

/**
 * A suite process outlives a Durable Object restart and the in-memory guard it held. Its
 * setup clears execution rows, so a second suite refuses while one lives, and a new suite
 * first ends every artifact process an earlier run left driving the bindings.
 */
export async function admitConformanceRun(exec: ContainerExec) {
  const suite = await exec(["/usr/bin/pgrep", "-f", "/usr/local/bin/contextful-conformance"]);
  if (suite.exitCode === 0) throw new Error("ConformanceAlreadyRunning");
  if (suite.exitCode !== 1) throw new Error("ConformanceContainerUnavailable");
  const swept = await exec(["/usr/bin/pkill", "-KILL", "-f", "/usr/local/bin/contextful"]);
  if (swept.exitCode !== 0 && swept.exitCode !== 1) throw new Error("ConformanceContainerUnavailable");
}

/** A changed container binding starts a fresh object with the current environment. */
export async function conformanceInstanceName(bindings: readonly string[]) {
  if (bindings.length !== 5 || !bindings.every((value) => typeof value === "string" && value.length > 0)) {
    throw new Error("ConformanceCapabilityMissing");
  }
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(JSON.stringify(bindings)));
  const suffix = Array.from(new Uint8Array(digest).slice(0, 16), (byte) => byte.toString(16).padStart(2, "0")).join("");
  return `conformance-${suffix}`;
}
