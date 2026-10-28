/** What the fixture builder and the measurement driver share on the operator's machine. */
import { createCipheriv } from "node:crypto";
import { spawn, spawnSync } from "node:child_process";
import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import { DAYS, dayOf, DIM_ROWS, TABLE } from "../src/plan.ts";

/** Bytes of seeded padding each row carries; base64 keeps them near-incompressible. */
export const PAD_BYTES = 768;

/** A deterministic byte stream: AES-128-CTR over zeros, keyed by `seed`. */
export class Stream {
  private cipher;
  constructor(seed: number, label: string) {
    const key = Buffer.alloc(16);
    key.writeUInt32BE(seed >>> 0, 0);
    Buffer.from(label).copy(key, 4, 0, 12);
    this.cipher = createCipheriv("aes-128-ctr", key, Buffer.alloc(16));
  }
  bytes(n: number): Buffer {
    return this.cipher.update(Buffer.alloc(n));
  }
  /** A uniform integer in `[0, bound)`. */
  below(bound: number): number {
    return Math.floor((this.bytes(4).readUInt32BE(0) / 2 ** 32) * bound);
  }
}

export type Row = { id: number; k: number; day: string; v: number; pad: string };

/** Event rows `[first, first + count)`, all on `day`; `k` cycles through the dimension keys. */
export function rows(stream: Stream, first: number, count: number, day: string): Row[] {
  const out: Row[] = [];
  for (let id = first; id < first + count; id++) {
    const block = stream.bytes(PAD_BYTES + 4);
    out.push({ id, k: id % DIM_ROWS, day, v: block.readUInt32BE(0) / 2 ** 32, pad: block.subarray(4).toString("base64") });
  }
  return out;
}

/** The dimension table: one row per `k`, grouped under 37 names. */
export function dimensionRows(): Array<{ k: number; name: string }> {
  return Array.from({ length: DIM_ROWS }, (_, k) => ({ k, name: `group-${String(k % 37).padStart(2, "0")}` }));
}

const runLine = (run: number, rows: unknown[]) => {
  const now = new Date(Date.UTC(2026, 0, 1) + run * 1000).toISOString();
  return JSON.stringify({ run_id: `run-${String(run).padStart(6, "0")}`, rows, now }) + "\n";
};

/** One `context land --session` input line: run `run` of `runs`, `count` rows from `first`. */
export function sessionLine(stream: Stream, run: number, runs: number, first: number, count: number): string {
  return runLine(run, rows(stream, first, count, dayOf(Math.floor((run * DAYS) / runs))));
}

/** The dimension table's single run. */
export const dimensionLine = () => runLine(0, dimensionRows());

/** Run `contextful` in `cwd` and return its stdout; a nonzero exit throws with stderr. */
export function contextful(bin: string, cwd: string, args: string[], env: NodeJS.ProcessEnv = process.env): string {
  const out = spawnSync(bin, args, { cwd, env, encoding: "utf8", maxBuffer: 64 << 20 });
  if (out.error) throw out.error;
  if (out.status !== 0) throw new Error(`contextful ${args.join(" ")} exited ${out.status}: ${out.stderr.trim()}`);
  return `${out.stdout}${out.stderr}`;
}

/** Feed `lines` to `contextful context land --session`, honouring stdin backpressure. */
export async function landSession(bin: string, cwd: string, project: string, lines: Iterable<string>, table = TABLE): Promise<void> {
  const child = spawn(bin, ["context", "land", table, "--project", project, "--site-id", "fixture", "--session"],
    { cwd, stdio: ["pipe", "ignore", "pipe"] });
  let stderr = "";
  child.stderr.on("data", (chunk) => { stderr = (stderr + chunk).slice(-4096); });
  const exited = new Promise<number | null>((resolve, reject) => {
    child.on("error", reject);
    child.on("close", resolve);
  });
  for (const line of lines) {
    if (!child.stdin.write(line)) await new Promise((resolve) => child.stdin.once("drain", resolve));
  }
  child.stdin.end();
  const code = await exited;
  if (code !== 0) throw new Error(`context land --session exited ${code}: ${stderr.trim()}`);
}

/** Files under `dir` whose name ends in `suffix`, with their sizes. */
export function filesUnder(dir: string, suffix: string): { count: number; bytes: number } {
  let count = 0;
  let bytes = 0;
  const stack = [dir];
  while (stack.length) {
    const at = stack.pop()!;
    for (const entry of readdirSync(at, { withFileTypes: true })) {
      const path = join(at, entry.name);
      if (entry.isDirectory()) stack.push(path);
      else if (entry.name.endsWith(suffix)) {
        count++;
        bytes += statSync(path).size;
      }
    }
  }
  return { count, bytes };
}

/** `--name value` pairs and bare `--flag`s. */
export function parseArgs(argv: string[]): Map<string, string> {
  const out = new Map<string, string>();
  for (let i = 0; i < argv.length; i++) {
    const key = argv[i];
    if (!key.startsWith("--")) throw new Error(`unexpected argument \`${key}\``);
    const value = argv[i + 1];
    if (value === undefined || value.startsWith("--")) out.set(key.slice(2), "true");
    else {
      out.set(key.slice(2), value);
      i++;
    }
  }
  return out;
}

/** The fixture manifest `fixtures.ts` writes and `measure.ts` reads. */
export type FixtureManifest = {
  seed: number;
  endpoint: string;
  bucket: string;
  scale: number;
  bytes_per_row: number;
  issuer_dir: string;
  public_key: string;
  fixtures: Array<{ name: string; prefix: string; runs: number; rows: number; objects: number; landed_bytes: number; parquet_bytes: number }>;
};
