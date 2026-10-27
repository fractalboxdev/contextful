/**
 * Build the hydration fixtures and push each to its own prefix of the corpus bucket.
 *
 *   npm run fixtures -- --endpoint r2://<account-id> --bucket <corpus-bucket> --prefix-root bench-<date>
 *   npm run fixtures -- --endpoint file://<abs-dir> --bucket corpus --scale 0.01      # loopback check
 *
 * A non-`file://` endpoint signs with CORPUS_R2_ACCESS_KEY_ID and CORPUS_R2_SECRET_ACCESS_KEY.
 * The manifest, issuer seed and public key land in `--out` (default ../../.tmp/cloudflare-query-bench).
 */
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { AUDIENCE, DIM_TABLE, FIXTURES, PROJECT, PROJECT_DECLARATION, SEED, planFixture, storeConfig, type Fixture, type SyncTarget } from "../src/plan.ts";
import { contextful, dimensionLine, filesUnder, landSession, parseArgs, sessionLine, Stream, type FixtureManifest } from "./support.ts";

const args = parseArgs(process.argv.slice(2));
const bin = args.get("contextful") ?? "contextful";
const endpoint = args.get("endpoint") ?? process.env.CORPUS_R2_ENDPOINT ?? "";
const bucket = args.get("bucket") ?? process.env.CORPUS_R2_BUCKET ?? "";
if (!endpoint || !bucket) throw new Error("FixtureArgsMissing: --endpoint and --bucket, or CORPUS_R2_ENDPOINT and CORPUS_R2_BUCKET, are required");
const root = args.get("prefix-root") ?? "bench";
const scale = Number(args.get("scale") ?? "1");
const out = resolve(args.get("out") ?? "../../.tmp/cloudflare-query-bench");
const selected = (args.get("fixtures") ?? FIXTURES.map((f) => f.name).join(",")).split(",");
const unknown = selected.filter((name) => !FIXTURES.some((f) => f.name === name));
if (unknown.length) throw new Error(`FixtureUnknown: ${unknown.join(", ")}`);
const signed = !endpoint.startsWith("file://");
const credentials = signed ? { accessKeyId: "env://CORPUS_R2_ACCESS_KEY_ID", secretAccessKey: "env://CORPUS_R2_SECRET_ACCESS_KEY" } : undefined;
if (signed && !(process.env.CORPUS_R2_ACCESS_KEY_ID && process.env.CORPUS_R2_SECRET_ACCESS_KEY)) {
  throw new Error("FixtureCredentialsMissing: set CORPUS_R2_ACCESS_KEY_ID and CORPUS_R2_SECRET_ACCESS_KEY");
}
mkdirSync(out, { recursive: true });

/** A project directory whose store pushes to `sync`. */
function project(dir: string, sync: SyncTarget | null) {
  mkdirSync(join(dir, ".contextful/context", PROJECT), { recursive: true });
  writeFileSync(join(dir, ".contextful/context", PROJECT, "config.toml"), storeConfig("fixture", sync, false));
  writeFileSync(join(dir, "contextful.toml"), PROJECT_DECLARATION);
}

// On-disk Parquet bytes of one row, from one landed calibration run.
const calibrationRows = 2_000;
const scratch = mkdtempSync(join(out, ".calibrate-"));
project(scratch, null);
await landSession(bin, scratch, PROJECT, [sessionLine(new Stream(SEED, "calibrate"), 0, 1, 0, calibrationRows)]);
const calibrated = filesUnder(join(scratch, ".contextful/context", PROJECT), ".parquet");
rmSync(scratch, { recursive: true, force: true });
const bytesPerRow = calibrated.bytes / calibrationRows;

// One issuer the query container pins and the driver mints read tokens from.
const issuerDir = join(out, "issuer");
mkdirSync(join(issuerDir, ".contextful"), { recursive: true });
writeFileSync(join(issuerDir, ".contextful/issuance.toml"), `default_audience = ${JSON.stringify(AUDIENCE)}\nmax_lifetime_secs = 3600\n`);
if (!existsSync(join(issuerDir, "issuer.pub"))) {
  const key = contextful(bin, issuerDir, ["token", "keygen", "--out", ".contextful/issuer.seed"]).trim();
  writeFileSync(join(issuerDir, "issuer.pub"), key + "\n");
}
const publicKey = readFileSync(join(issuerDir, "issuer.pub"), "utf8").trim();

const manifest: FixtureManifest = {
  seed: SEED, endpoint, bucket, scale, bytes_per_row: bytesPerRow, issuer_dir: issuerDir, public_key: publicKey, fixtures: [],
};
/** Land one fixture's event runs and its dimension table, then push them to its prefix. */
async function build(fixture: Fixture): Promise<FixtureManifest["fixtures"][number]> {
  const plan = planFixture(fixture, bytesPerRow, scale);
  const prefix = `${root}/${fixture.name}`;
  const dir = join(out, "stores", fixture.name);
  rmSync(dir, { recursive: true, force: true });
  project(dir, { endpoint, bucket, prefix, lazyParts: false, credentials });
  const stream = new Stream(SEED, fixture.name);
  function* runs() {
    for (let run = 0; run < plan.runs; run++) yield sessionLine(stream, run, plan.runs, run * plan.rowsPerRun, plan.rowsPerRun);
  }
  const started = Date.now();
  await landSession(bin, dir, PROJECT, runs());
  await landSession(bin, dir, PROJECT, [dimensionLine()], DIM_TABLE);
  const pushed = /pushed (\d+) objects/.exec(contextful(bin, dir, ["sync", "push", "--project", PROJECT]));
  const parquet = filesUnder(join(dir, ".contextful/context", PROJECT), ".parquet");
  const entry = {
    name: fixture.name, prefix, runs: plan.runs, rows: plan.runs * plan.rowsPerRun,
    objects: pushed ? Number(pushed[1]) : -1, parquet_bytes: parquet.bytes,
  };
  console.log(`${fixture.name}: ${entry.rows} rows in ${entry.runs} runs, ${entry.objects} objects, ` +
    `${(entry.parquet_bytes / 1e6).toFixed(1)} MB Parquet, built and pushed in ${((Date.now() - started) / 1000).toFixed(1)} s`);
  return entry;
}

// Each fixture lands in its own process, so the fixtures build concurrently.
manifest.fixtures = await Promise.all(FIXTURES.filter((f) => selected.includes(f.name)).map(build));
writeFileSync(join(out, "fixtures.json"), JSON.stringify(manifest, null, 2) + "\n");
console.log(`issuer public key (the ISSUER_PUBLIC_KEY secret): ${publicKey}`);
