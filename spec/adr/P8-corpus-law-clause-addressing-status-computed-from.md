# P8 — Clauses are addressed, status is computed from pins, and rationale lives in records

**Status:** accepted

## Context

A prose specification restates a fact in several places, and the copies drift. A status marker an author writes is correct the day it is written. Rationale interleaved with behavior ages at a different rate from the behavior and turns contracts into arguments.

## Decision

The corpus is data a checker reads, and every rule it states is one the checker enforces.

- A normative sentence is one clause item addressed `<contract>.<operation>.<subject>`. Contract and operation resolve against the registry; the kind is computed from the error and bound entries the fragment assigns. An id changes only when a fact changes obligor.
- A fact has one home, and another statement reaches it by `{{id}}`.
- `spec/status.md` is generated from `spec/pins.toml`: an unpinned clause is committed, a resolving pin is performed, and a pin that resolves to nothing is broken and reds the gate. No authored file states build state.
- Rationale lives in principle records and one ADR per contract under `spec/adr/`. A clause's Why cites a record or carries a short deciding criterion; nothing else in a contract argues.
- An unknown is one inline line where it applies, naming a question, an owner and an operation.
- `corpus.rationale` and the other corpus operations are enforced by one command, which the gate and a local run invoke identically.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Addressed clauses, computed status, rationale in records *(chosen)* | — | Every new fact needs a registry entry, every performed claim a pin, and every contested direction a record. |
| Prose sections with a citation convention | Detection | A citation records acknowledgement; nothing joins a restated fact back to its owner. |
| Status markers written beside each claim | Maintenance | A marker is stale the day the code moves. |
| Status from whether a named identifier exists, with no pin | Precision | A type existing is not evidence that a refusal fires. |
| Rationale inline, marked off by convention | Rot | Argument and behavior age at different rates, and the argument ends up contradicting the rule. |

## Consequences

- A rename in code reds the gate through its pin, not through a reader noticing.
- Moving an operation between files of one contract renames nothing.
- A record carries the full argument, so a clause stays within its word limit.
