import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

type Scenario = { id: string; required: boolean; outcome: string };
type Report = {
  artifact_digest: string;
  source_commit: string;
  source_dirty: boolean;
  suite_revision: string;
  storage_profile: string;
  feature_profile: string;
  binding_origin: string;
  compatible: boolean;
  suite_exit_code?: number;
  scenarios: Scenario[];
};
type Release = {
  profile: string;
  archive: string;
  sha256: string;
  d1_compatibility: {
    report: string;
    artifact_digest: string;
    source_commit: string;
    suite_revision: string;
    storage_profile: string;
    binding_origin?: string;
    compatible: boolean;
    scenarios?: Scenario[];
  };
};

const digest = (path: string) => createHash("sha256").update(readFileSync(path)).digest("hex");
const runTar = (args: string[], cwd?: string) => {
  const result = spawnSync("tar", args, { cwd, encoding: "utf8" });
  if (result.status !== 0) throw new Error("ConformanceReleaseArchiveInvalid");
};

/** A passing provider report replaces one release cell's incompatible D1 evidence. */
export function attachConformanceReport(releasePath: string, providerPath: string, outputPath: string): void {
  const metadataPath = resolve(releasePath);
  const output = resolve(outputPath);
  const sourceDir = dirname(metadataPath);
  const release = JSON.parse(readFileSync(metadataPath, "utf8")) as Release;
  const provider = JSON.parse(readFileSync(providerPath, "utf8")) as Report;
  const archivePath = join(sourceDir, release.archive);
  const stem = basename(release.archive).replace(/\.tar\.gz$/, "");
  const baselinePath = join(sourceDir, release.d1_compatibility.report);
  const baseline = JSON.parse(readFileSync(baselinePath, "utf8")) as Report;
  const expectedIds = baseline.scenarios.map((scenario) => scenario.id).sort();
  const actualIds = provider.scenarios.map((scenario) => scenario.id).sort();
  if (release.profile !== "contextful-full" || !release.archive.endsWith(".tar.gz") ||
      release.d1_compatibility.report !== `${stem}.cloudflare-d1.compatibility.json` ||
      digest(archivePath) !== release.sha256 ||
      baseline.artifact_digest !== release.d1_compatibility.artifact_digest ||
      baseline.source_commit !== release.d1_compatibility.source_commit ||
      baseline.suite_revision !== release.d1_compatibility.suite_revision ||
      baseline.storage_profile !== "cloudflare-d1" || baseline.source_dirty !== false ||
      provider.artifact_digest !== release.d1_compatibility.artifact_digest ||
      provider.source_commit !== release.d1_compatibility.source_commit ||
      provider.suite_revision !== release.d1_compatibility.suite_revision ||
      provider.storage_profile !== "cloudflare-d1" ||
      provider.feature_profile !== "contextful-full" ||
      provider.binding_origin !== "cloudflare-worker" ||
      provider.source_dirty !== false || provider.suite_exit_code !== 0 ||
      provider.compatible !== true || expectedIds.length === 0 ||
      JSON.stringify(expectedIds) !== JSON.stringify(actualIds) ||
      !provider.scenarios.every((scenario) => scenario.required && scenario.outcome === "passed")) {
    throw new Error("ConformanceReleaseEvidenceMismatch");
  }
  if (existsSync(output)) throw new Error("ConformanceReleaseOutputExists");
  mkdirSync(dirname(output), { recursive: true });
  const staged = mkdtempSync(join(dirname(output), ".contextful-release-"));
  try {
    const unpacked = join(staged, "unpacked");
    mkdirSync(unpacked);
    runTar(["-xzf", archivePath, "-C", unpacked]);
    writeFileSync(join(unpacked, stem, "cloudflare-d1.compatibility.json"), JSON.stringify(provider, null, 2) + "\n");
    for (const name of readdirSync(sourceDir).filter((name) => name.startsWith(`${stem}.`))) {
      if (![release.archive, `${release.archive}.sha256`, `${stem}.release.json`, release.d1_compatibility.report].includes(name)) {
        copyFileSync(join(sourceDir, name), join(staged, name));
      }
    }
    runTar(["-czf", join(staged, release.archive), stem], unpacked);
    const archiveDigest = digest(join(staged, release.archive));
    writeFileSync(join(staged, `${release.archive}.sha256`), `${archiveDigest}  ${release.archive}\n`);
    writeFileSync(join(staged, release.d1_compatibility.report), JSON.stringify(provider, null, 2) + "\n");
    writeFileSync(join(staged, `${stem}.release.json`), JSON.stringify({
      ...release,
      sha256: archiveDigest,
      d1_compatibility: { ...release.d1_compatibility, binding_origin: provider.binding_origin,
        compatible: true, scenarios: provider.scenarios },
    }, null, 2) + "\n");
    rmSync(unpacked, { recursive: true });
    renameSync(staged, output);
  } finally {
    if (existsSync(staged)) rmSync(staged, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [release, provider, output] = process.argv.slice(2);
  if (!release || !provider || !output) {
    process.stderr.write("usage: attach-report <release.json> <provider-report.json> <output-dir>\n");
    process.exitCode = 2;
  } else {
    attachConformanceReport(release, provider, output);
  }
}
