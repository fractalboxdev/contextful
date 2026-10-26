# P3 — A misconfigured process refuses to start; one value has one source

**Status:** accepted

## Context

A process that starts under a configuration it cannot honor runs in a mode nobody chose, while its health check reads green. Two sources for one value hide which one won.

## Decision

A process resolves every value it needs at startup, before it binds a port, opens a table or writes a row. An unset, malformed or unusable value refuses startup, naming the input and, where one exists, the fixing command. A value has exactly one declared source; declaring two refuses, and no fallback fabricates a value.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Refuse startup; one source per value *(chosen)* | — | A deployment with one bad value serves nothing, including the correct parts. |
| Fall back to a default or generated value | Failure shape | The process runs healthy in an undeclared mode. |
| Accept two sources under a precedence rule | Ambiguity | An operator reading one source does not know the other overrode it. |

## Consequences

- A health check reports up only for a process that can serve.
- An unconfigured owner and an uninitialized store answer as distinct refusals, so an operator knows which to fix.
