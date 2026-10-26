# P5 — Rows are reached only through the caller's registered relation; one implementation per capability

**Status:** accepted

## Context

Enforcement holds only if it is always invoked. A path around the enforced relation is where predicates and masks stop applying, and a second implementation of a rule drifts.

## Decision

Every read of stored rows, caller-facing or internal, traverses the caller's registered relation. The engine compiles grant filters, masks and a visibility semi-join into that relation. A path around it is a defect, never a documented exception. Each enforcement capability has one implementation.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| One enforced relation, one implementation per capability *(chosen)* | — | No fast path around enforcement; a gateway carries a WebAssembly build of the engine module. |
| Exempt a documented set of internal paths | Coverage | The exemption list is where rows escape, and it grows with each maintenance feature. |
| A filter per surface, held in line by a conformance suite | Failure mode | Two implementations diverge on the case the suite omits. |

## Consequences

- A surface that cannot call the engine implements an adapter against its ports, never a restatement of its contract.
- A visibility audit reads one binding per table.
