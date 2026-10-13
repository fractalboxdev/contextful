import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, renameSync, rmSync, closeSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import type { ReleaseProvenance } from "./conformance.ts";

type Release = {
  profile: string;
  target: string;
  archive: string;
  sha256: string;
  d1_compatibility: ReleaseProvenance;
};

type D1Report = ReleaseProvenance & { source_dirty: boolean };

const digest = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");

/** The image contains the exact release binary and suite, never a second build. */
export function prepareConformanceImage(archivePath: string, releasePath: string, reportPath: string, outputPath: string): void {
  const archive = resolve(archivePath);
  const output = resolve(outputPath);
  const release = JSON.parse(readFileSync(releasePath, "utf8")) as Release;
  const report = JSON.parse(readFileSync(reportPath, "utf8")) as D1Report;
  const stem = basename(archive).replace(/\.tar\.gz$/, "");
  if (!archive.endsWith(".tar.gz") || release.archive !== basename(archive) ||
      release.profile !== "contextful-full" || release.target !== "x86_64-unknown-linux-musl" ||
      digest(readFileSync(archive)) !== release.sha256 || report.source_dirty !== false ||
      !/^[a-f0-9]{40}$/.test(report.source_commit) ||
      report.artifact_digest !== release.d1_compatibility.artifact_digest ||
      report.source_commit !== release.d1_compatibility.source_commit ||
      report.suite_revision !== release.d1_compatibility.suite_revision) {
    throw new Error("ConformanceReleaseEvidenceMismatch");
  }
  if (existsSync(output)) throw new Error("ConformanceImageContextExists");
  mkdirSync(dirname(output), { recursive: true });
  const staged = mkdtempSync(join(dirname(output), ".contextful-image-"));
  try {
    for (const name of ["contextful", "contextful-conformance"]) {
      const descriptor = openSync(join(staged, name), "w", 0o755);
      try {
        const result = spawnSync("tar", ["-xOzf", archive, `${stem}/${name}`], {
          stdio: ["ignore", descriptor, "pipe"],
        });
        if (result.status !== 0) throw new Error("ConformanceReleaseArchiveInvalid");
      } finally {
        closeSync(descriptor);
      }
    }
    if (digest(readFileSync(join(staged, "contextful"))) !== report.artifact_digest) {
      throw new Error("ConformanceReleaseEvidenceMismatch");
    }
    copyFileSync(fileURLToPath(new URL("../Dockerfile.conformance", import.meta.url)), join(staged, "Dockerfile"));
    writeFileSync(join(staged, "source.json"), JSON.stringify({
      artifact_digest: report.artifact_digest,
      source_commit: report.source_commit,
      suite_revision: report.suite_revision,
    }) + "\n");
    renameSync(staged, output);
  } finally {
    if (existsSync(staged)) rmSync(staged, { recursive: true, force: true });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const [archive, release, report, output] = process.argv.slice(2);
  if (!archive || !release || !report || !output) {
    process.stderr.write("usage: prepare-image <release.tar.gz> <release.json> <cloudflare-d1.compatibility.json> <image-context>\n");
    process.exitCode = 2;
  } else {
    prepareConformanceImage(archive, release, report, output);
  }
}
