# P3 — A misconfigured process refuses to start; one value has one source

**Status:** accepted

## Context

A process that starts under a configuration it cannot honor runs in a mode nobody chose: cleartext where a key was declared, the bucket root where a prefix was meant, a fabricated signing key, a non-durable trigger. Its health check reads green while callers fail. Two declared sources for one value turn every incident into a question about which one won.

## Decision

A process resolves every value it needs at startup, before it binds a port, opens a table or lands a row. An unset, malformed or unusable value refuses startup, naming the input and, where one exists, the fixing command. A value has exactly one declared source; declaring two refuses, and no fallback fabricates a value.

- `store.lay-out` resolves the writer node id from the environment, then project configuration, then a random id persisted outside the store root, checked against `^[A-Za-z0-9._-]{1,64}$` before any directory exists.
- `store.encrypt` refuses startup on an unresolvable key source and accepts no literal key.
- `store.push` binds the bucket prefix from one source and confines keys to it inside the object-store wrapper.
- `connector.resolve` resolves an enumerated set of environment bindings, each with a declared value shape.
- `authority.verify` refuses to start a face with no usable issuer key and generates none.
- `surface.arm` refuses an unknown trigger adapter rather than downgrading.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse startup; one source per value *(chosen)* | — | A deployment with one bad value serves nothing, including the parts that were correct. |
| Fall back to a default or generated value | Failure shape | The process runs healthy in an undeclared mode, and the fault surfaces as a data or security incident. |
| Defer the check to first use | Failure placement | The refusal lands mid-run, after work was accepted. |
| Accept two sources under a precedence rule | Ambiguity | An operator reading one source does not know the other overrode it. |
| Disable the offending resource and run degraded | Legibility | Callers meet a partial surface shaped by whichever value was wrong. |

## Consequences

- A health check reports up only for a process that can serve.
- An unconfigured owner and an uninitialized store answer as distinct refusals, so an operator knows which to fix.
