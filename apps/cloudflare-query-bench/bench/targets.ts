/** Where a measurement runs: a deployed bench Worker, or a loopback `contextful serve`. */
import { spawn, type ChildProcess } from "node:child_process";
import { closeSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { join } from "node:path";
import type { ProbeResult, QueryResult, StartRequest } from "../src/container.ts";

import { AUDIENCE, healthHolds, healthPath, parseServeLog, PROJECT, PROJECT_DECLARATION, QUERY_TOKEN_HEADER, storeConfig } from "../src/plan.ts";

export interface Target {
  /** Stop any running container and start a fresh one on an empty disk. */
  begin(request: StartRequest): Promise<void>;
  /** Wait at most `waitMs` for readiness or hydration; elapsed counts from `begin`. */
  probe(until: "ready" | "hydrated", waitMs: number): Promise<ProbeResult>;
  /** One MCP request through the routing layer, which starts a stopped container. */
  query(body: string, token: string): Promise<QueryResult>;
  /** Leave the container idle until it sleeps. */
  idle(sleepAfterSecs: number): Promise<void>;
  close(): Promise<void>;
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** The bench Worker's `/bench/*` routes under `url`, authenticated by `token`. */
export class CloudflareTarget implements Target {
  private url: string;
  private token: string;

  constructor(url: string, token: string) {
    this.url = url;
    this.token = token;
  }

  private async post(path: string, body: string, headers: Record<string, string> = {}): Promise<Response> {
    return fetch(new URL(path, this.url), {
      method: "POST", body, headers: { Authorization: `Bearer ${this.token}`, "Content-Type": "application/json", ...headers },
    });
  }

  private async json<T>(path: string, body: unknown): Promise<T> {
    const answer = await this.post(path, JSON.stringify(body));
    if (!answer.ok) throw new Error(`${path} answered ${answer.status}: ${await answer.text()}`);
    return answer.json() as Promise<T>;
  }

  async begin(request: StartRequest) {
    await this.json("/bench/start", request);
  }

  async probe(until: "ready" | "hydrated", waitMs: number): Promise<ProbeResult> {
    const deadline = Date.now() + waitMs;
    for (;;) {
      const result = await this.json<ProbeResult>("/bench/probe", { until, wait_ms: Math.min(20_000, Math.max(0, deadline - Date.now())) });
      if (result.held || Date.now() >= deadline) return result;
    }
  }

  async query(body: string, token: string): Promise<QueryResult> {
    const answer = await this.post("/bench/query", body, { [QUERY_TOKEN_HEADER]: `Bearer ${token}` });
    return { status: answer.status, retry_after: answer.headers.get("Retry-After"), body: await answer.text() };
  }

  async idle(sleepAfterSecs: number) {
    await sleep((sleepAfterSecs + 30) * 1000);
  }

  async close() {}
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as { port: number }).port;
      server.close(() => resolve(port));
    });
  });
}

/** `contextful serve` on loopback against the fixtures' `file://` bucket; idling stops the process. */
export class LocalTarget implements Target {
  private child: ChildProcess | null = null;
  private startedAt = 0;
  private port = 0;
  private request: StartRequest | null = null;

  private bin: string;
  private workDir: string;
  private endpoint: string;
  private bucket: string;
  private publicKey: string;

  constructor(bin: string, workDir: string, endpoint: string, bucket: string, publicKey: string) {
    this.bin = bin;
    this.workDir = workDir;
    this.endpoint = endpoint;
    this.bucket = bucket;
    this.publicKey = publicKey;
  }

  private async spawnServe(request: StartRequest) {
    await this.stop();
    rmSync(this.workDir, { recursive: true, force: true });
    const store = join(this.workDir, ".contextful/context", PROJECT);
    mkdirSync(store, { recursive: true });
    const sync = request.prefix === null ? null : { endpoint: this.endpoint, bucket: this.bucket, prefix: request.prefix, lazyParts: request.lazy_parts };
    writeFileSync(join(store, "config.toml"), storeConfig("query", sync));
    writeFileSync(join(this.workDir, "contextful.toml"), PROJECT_DECLARATION);
    this.port = await freePort();
    const log = openSync(join(this.workDir, "serve.log"), "w");
    this.startedAt = performance.now();
    this.child = spawn(this.bin, ["serve", "--http", `127.0.0.1:${this.port}`, "--project", PROJECT,
      "--public-key", this.publicKey, "--audience", AUDIENCE, "--max-in-flight", "8"],
      { cwd: this.workDir, stdio: ["ignore", "ignore", log] });
    closeSync(log);
  }

  private async stop() {
    const child = this.child;
    this.child = null;
    if (child && child.exitCode === null) {
      const exited = new Promise((resolve) => child.once("exit", resolve));
      child.kill("SIGKILL");
      await exited;
    }
  }

  async begin(request: StartRequest) {
    this.request = request;
    await this.spawnServe(request);
  }

  async probe(until: "ready" | "hydrated", waitMs: number): Promise<ProbeResult> {
    const deadline = performance.now() + waitMs;
    for (;;) {
      try {
        const answer = await fetch(`http://127.0.0.1:${this.port}${healthPath(until)}`);
        if (healthHolds(until, answer.status, await answer.text())) {
          const elapsed_ms = performance.now() - this.startedAt;
          const log = until === "hydrated" ? parseServeLog(readFileSync(join(this.workDir, "serve.log"), "utf8")) : null;
          return { held: true, elapsed_ms, log };
        }
      } catch {
        // Not listening yet.
      }
      if (performance.now() >= deadline) return { held: false, elapsed_ms: performance.now() - this.startedAt, log: null };
      await sleep(50);
    }
  }

  async query(body: string, token: string): Promise<QueryResult> {
    if (!this.child && this.request) await this.spawnServe(this.request);
    try {
      const answer = await fetch(`http://127.0.0.1:${this.port}/mcp`, {
        method: "POST", body, headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
      });
      return { status: answer.status, retry_after: answer.headers.get("Retry-After"), body: await answer.text() };
    } catch {
      return { status: 503, retry_after: "1", body: "" };
    }
  }

  async idle() {
    await this.stop();
  }

  async close() {
    await this.stop();
  }
}
