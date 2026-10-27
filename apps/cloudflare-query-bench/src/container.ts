import { Container } from "@cloudflare/containers";
import { AUDIENCE, healthHolds, healthPath, parseServeLog, PROJECT, PROJECT_DECLARATION, SERVE_PORT, storeConfig, type HydrationLog } from "./plan.ts";

/** Which fixture a cold start hydrates; `prefix: null` starts on an empty disk with no bucket. */
export type StartRequest = { prefix: string | null; lazy_parts: boolean };
export type ProbeResult = { held: boolean; elapsed_ms: number; log: HydrationLog | null };
export type QueryResult = { status: number; retry_after: string | null; body: string };

const PREFIX = /^[a-z0-9][a-z0-9/_-]{0,127}$/;
const POLL_MS = 50;
const STOP_WAIT_MS = 30_000;
const LOG_LIMIT = 65_536;
const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * A Durable Object-managed container application names its images in `ctx.container.images`
 * and takes the image and instance type at each start; `@cloudflare/containers` passes
 * neither, so every start this object makes carries the first image and `instance`.
 */
function pinImage(runtime: DurableObjectState["container"], instance: ContainerStartupOptions["instance"]) {
  let images: Record<string, string> = {};
  try {
    images = runtime?.images ?? {};
  } catch {
    return;
  }
  const image = Object.values(images)[0];
  if (!runtime || !image) return;
  const start = runtime.start.bind(runtime);
  Object.defineProperty(runtime, "start", {
    configurable: true,
    writable: true,
    value: (options?: ContainerStartupOptions) => start({ enableInternet: true, ...options, image, instance } as ContainerStartupOptions),
  });
}

/** One `contextful serve --http` query container whose cold starts the bench times. */
export class QueryBenchContainer extends Container<QueryBenchEnv> {
  defaultPort = SERVE_PORT;
  enableInternet = true;
  private startedAt = 0;

  constructor(ctx: DurableObjectState<{}>, env: QueryBenchEnv) {
    pinImage(ctx.container, (env.QUERY_INSTANCE_TYPE || "standard-1") as ContainerStartupOptions["instance"]);
    super(ctx, env);
    this.sleepAfter = env.QUERY_SLEEP_AFTER || "5m";
    ctx.blockConcurrencyWhile(async () => {
      const held = await ctx.storage.get<StartRequest>("start");
      if (held) this.envVars = this.containerEnv(held);
    });
  }

  private containerEnv(request: StartRequest): Record<string, string> {
    const sync = request.prefix === null ? null : {
      endpoint: this.env.CORPUS_R2_ENDPOINT,
      bucket: this.env.CORPUS_R2_BUCKET,
      prefix: request.prefix,
      lazyParts: request.lazy_parts,
      credentials: { accessKeyId: "env://CORPUS_R2_ACCESS_KEY_ID", secretAccessKey: "env://CORPUS_R2_SECRET_ACCESS_KEY" },
    };
    return {
      BENCH_PROJECT: PROJECT,
      BENCH_AUDIENCE: AUDIENCE,
      BENCH_PORT: String(SERVE_PORT),
      BENCH_DECLARATION: PROJECT_DECLARATION,
      BENCH_STORE_CONFIG: storeConfig("query", sync),
      CORPUS_R2_ACCESS_KEY_ID: this.env.CORPUS_R2_ACCESS_KEY_ID,
      CORPUS_R2_SECRET_ACCESS_KEY: this.env.CORPUS_R2_SECRET_ACCESS_KEY,
      ISSUER_PUBLIC_KEY: this.env.ISSUER_PUBLIC_KEY,
    };
  }

  /** Stop any running instance, then start a fresh one on an empty disk; the clock starts here. */
  async begin(request: StartRequest): Promise<{ stopped_ms: number }> {
    if (request.prefix !== null && !PREFIX.test(request.prefix)) throw new Error("BenchPrefixInvalid");
    const stopping = Date.now();
    if (this.ctx.container?.running) {
      await this.destroy();
      while (this.ctx.container?.running) {
        if (Date.now() - stopping > STOP_WAIT_MS) throw new Error("BenchContainerStillRunning");
        await sleep(POLL_MS);
      }
    }
    const stopped_ms = Date.now() - stopping;
    await this.ctx.storage.put("start", request);
    this.envVars = this.containerEnv(request);
    this.startedAt = Date.now();
    await this.start({ envVars: this.envVars, enableInternet: true });
    return { stopped_ms };
  }

  /**
   * Poll `/health` (`ready`) or `/health?ready=hydrated` (`hydrated`) for at most `wait_ms`;
   * `elapsed_ms` counts from the last `begin`. A held `hydrated` probe carries the serve log's
   * pull and mapping figures.
   */
  async probe(until: "ready" | "hydrated", wait_ms: number): Promise<ProbeResult> {
    if (this.startedAt === 0) throw new Error("BenchNotStarted");
    const deadline = Date.now() + Math.min(Math.max(wait_ms, 0), 25_000);
    for (;;) {
      if (this.ctx.container?.running) {
        try {
          const answer = await this.containerFetch(`http://query${healthPath(until)}`, { method: "GET" }, SERVE_PORT);
          if (healthHolds(until, answer.status, await answer.text())) {
            const elapsed_ms = Date.now() - this.startedAt;
            return { held: true, elapsed_ms, log: until === "hydrated" ? await this.serveLog() : null };
          }
        } catch {
          // The port is not listening yet.
        }
      }
      if (Date.now() >= deadline) return { held: false, elapsed_ms: Date.now() - this.startedAt, log: null };
      await sleep(POLL_MS);
    }
  }

  /** Forward one MCP request as the routing Worker does, starting a stopped container first. */
  async query(body: string, token: string): Promise<QueryResult> {
    try {
      const answer = await this.containerFetch(`http://query/mcp`, {
        method: "POST",
        headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` },
        body,
      }, SERVE_PORT);
      return { status: answer.status, retry_after: answer.headers.get("Retry-After"), body: await answer.text() };
    } catch {
      return { status: 503, retry_after: "1", body: "" };
    }
  }

  private async serveLog(): Promise<HydrationLog | null> {
    const container = this.ctx.container;
    if (!container) return null;
    try {
      const output = await (await container.exec(["/bin/cat", "/work/serve.log"])).output();
      if (output.exitCode !== 0) return null;
      const bytes = new Uint8Array(output.stdout);
      return parseServeLog(new TextDecoder().decode(bytes.subarray(Math.max(0, bytes.byteLength - LOG_LIMIT))));
    } catch {
      return null;
    }
  }
}
