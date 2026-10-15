# `@contextful/cloudflare`

A call-mode delegate for **Contextful** store-driven items on a Cloudflare Worker. The daemon binds the Worker under `[control.delegates.<name>]`, posts one signed envelope per item attempt, and lands the rows the Worker answers with. The package verifies the request, runs your handler, and signs the answer; the engine's `surface.delegate` clauses are the contract, and every number or prefix restated here names its clause.

```ts
import { delegate, ok, defer, permanent } from "@contextful/cloudflare";

export default {
  fetch: delegate<{ CONTEXTFUL_WORKER_KEY: string }>(
    async (envelope) => {
      const { entity_id, pass_id } = envelope.row as { entity_id: string; pass_id: string };
      const verdict = await screen(entity_id);
      if (verdict.budgetExhausted) return defer(verdict.retryAt, "provider budget");
      if (verdict.withdrawn) return permanent("subject withdrawn");
      return ok({ screen_result: [{ entity_id, pass_id, proceed: verdict.proceed }] });
    },
    { key: (env) => env.CONTEXTFUL_WORKER_KEY },
  ),
};
```

The daemon side:

```toml
[control.delegates.stages]
url     = "http://contextful.delegate/stages"   # the Worker's outbound interception, or a loopback relay
mode    = "call"
timeout = "300s"
version = "stage-prompts-v3"                    # rotate when the handler's semantics change

[[job]]
name           = "stage-screen"
kind           = "store-driven"
body           = { delegate = "stages" }
statement      = "SELECT entity_id, pass_id FROM due_items"
max_in_flight  = 5
tables         = ["screen_result"]
isolated_items = { logical_key = "pass_id", entity_key = "entity_id", entity_namespace = "entity", max_attempts = 3 }
```

## What the handler sees

`envelope.row` is the input row exactly as the read face projects it, so a BIGINT or DECIMAL arrives as a string. `envelope.entity` carries the namespace, key and lease fence of an isolated item; `envelope.pins` the execution's owner pins, including `body_dependency_delegate_version` when the binding declares one; `envelope.version` only when the job fires from an applied snapshot; `envelope.deadline` the engine's instant plus the binding's timeout.

## What the answer means

| Answer | The engine |
| --- | --- |
| `ok({ table: rows })` | lands the rows as the item's rows and writes its completion receipt |
| `defer(retryAt)` | parks the item until `retryAt`, spending no attempt; the first fire after it asks again under the next ordinal |
| `transient()` | spends one failed attempt; the next fire asks again |
| `permanent(reason)` | writes a terminal receipt |
| a thrown error, or any non-200 | spends one failed attempt (`surface.delegate.status`) |
| rows for a table outside `tables` | never leave the Worker: `respond` refuses with 500 |

One request per item attempt: the engine runs no retry inside an attempt, so the item budget `max_attempts` is the only retry layer. Signature timestamps ride each side's system clock within 300 s; the deadline and a deferral ride the engine's instant, which can sit years from the wall clock under an injected `--now`. Budget a model call with `timeToDeadline(envelope)`, which measures the deadline against the envelope's own `as_of` and clamps the result to what a timer takes, never `deadline - Date.now()`.

## Running the handler in-process during tests

A test that hosts the delegate in the same process as the CLI must not call the CLI synchronously (`spawnSync`) while a request is in flight: the Worker cannot answer until the CLI returns, and the engine closes the attempt at the binding's timeout. Spawn the CLI asynchronously, or host the delegate on its own loopback port.

## Not in this package

`mode = "submit"` (the worker protocol with heartbeat and callback, for work that outlives one connection) and the Workflow filler of the orchestrator role (`run.orchestrate`) are later slices; a `submit` binding refuses at fire until they land.
