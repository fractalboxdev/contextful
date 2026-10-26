export { QueryBenchContainer } from "./container.ts";
export { ContainerProxy } from "@cloudflare/containers";
import type { StartRequest } from "./container.ts";
import { QUERY_TOKEN_HEADER, sameSecret } from "./plan.ts";

const missing = () => new Response(null, { status: 404 });

/**
 * Three bench-token routes, each `POST`: `/bench/start` begins a cold start, `/bench/probe`
 * long-polls readiness or hydration, and `/bench/query` forwards one MCP request to the
 * query container. Every other request answers `404`.
 */
export default {
  async fetch(request: Request, env: QueryBenchEnv): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "POST" || !env.BENCH_TOKEN) return missing();
    if (!sameSecret(request.headers.get("authorization") ?? "", `Bearer ${env.BENCH_TOKEN}`)) return missing();
    const container = env.QUERY_CONTAINER.get(env.QUERY_CONTAINER.idFromName("query-bench"));
    try {
      switch (url.pathname) {
        case "/bench/start":
          return Response.json(await container.begin(await request.json() as StartRequest));
        case "/bench/probe": {
          const { until, wait_ms } = await request.json() as { until: "ready" | "hydrated"; wait_ms: number };
          if (until !== "ready" && until !== "hydrated") return Response.json({ error: "BenchProbeInvalid" }, { status: 400 });
          return Response.json(await container.probe(until, wait_ms));
        }
        case "/bench/query": {
          const token = (request.headers.get(QUERY_TOKEN_HEADER) ?? "").replace(/^Bearer /, "");
          const answer = await container.query(await request.text(), token);
          const headers = new Headers({ "Content-Type": "application/json" });
          if (answer.retry_after) headers.set("Retry-After", answer.retry_after);
          return new Response(answer.body, { status: answer.status, headers });
        }
        default:
          return missing();
      }
    } catch (error) {
      console.error("BenchRequestFailed", error instanceof Error ? `${error.name}: ${error.message}` : typeof error);
      return Response.json({ error: "BenchRequestFailed" }, { status: 503 });
    }
  },
} satisfies ExportedHandler<QueryBenchEnv>;
