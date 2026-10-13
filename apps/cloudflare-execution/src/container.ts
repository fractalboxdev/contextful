import { Container } from "@cloudflare/containers";
import { handleExecutionBinding, type ExecutionBindings } from "./binding.ts";

/** The Container's outbound virtual host resolves D1 and R2 inside its Worker. */
export class D1ExecutionContainer extends Container<ContextfulExecutionEnv> {
  static outboundByHost = {
    "contextful.d1": (request: Request, env: ExecutionBindings) =>
      handleExecutionBinding(request, env),
  };
}
