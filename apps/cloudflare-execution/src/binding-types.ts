import type { ExecutionBindings } from "./binding.ts";

declare const bindings: { EXECUTION_DB: D1Database; EXECUTION_BLOBS: R2Bucket };
const compatible: ExecutionBindings = bindings;
void compatible;
