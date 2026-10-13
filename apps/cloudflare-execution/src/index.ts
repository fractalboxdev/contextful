export { D1ExecutionContainer } from "./container.ts";

/** The Worker hosts the Container and exposes no execution storage over HTTP. */
export default {
  fetch(): Response {
    return new Response(null, { status: 404 });
  },
} satisfies ExportedHandler<ContextfulExecutionEnv>;
