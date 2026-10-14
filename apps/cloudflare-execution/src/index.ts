export { D1ExecutionContainer } from "./container.ts";
export { ContainerProxy } from "@cloudflare/containers";

const missing = () => new Response(null, { status: 404 });

/** The Worker exposes one credential-checked conformance route and no storage route. */
export default {
  async fetch(request: Request, env: ContextfulExecutionEnv): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "POST" || url.pathname !== "/conformance") return missing();
    if (!env.CONFORMANCE_TOKEN) return missing();
    const supplied = new TextEncoder().encode(request.headers.get("authorization") ?? "");
    const expected = new TextEncoder().encode(`Bearer ${env.CONFORMANCE_TOKEN}`);
    if (supplied.byteLength !== expected.byteLength ||
        !crypto.subtle.timingSafeEqual(supplied, expected)) return missing();
    try {
      const id = env.EXECUTION_CONTAINER.idFromName("conformance");
      return Response.json(await env.EXECUTION_CONTAINER.get(id).runConformance());
    } catch (error) {
      console.error("ConformanceRunFailed", error instanceof Error ? `${error.name}: ${error.message}` : typeof error);
      return Response.json({ error: "ConformanceRunFailed" }, { status: 503 });
    }
  },
} satisfies ExportedHandler<ContextfulExecutionEnv>;
