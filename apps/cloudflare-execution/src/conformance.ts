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
