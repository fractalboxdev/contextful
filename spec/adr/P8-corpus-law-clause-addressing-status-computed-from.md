# P8 — Clauses are addressed, status is computed from pins, and rationale lives in records

**Status:** accepted

## Context

Restated facts drift, an authored status marker goes stale, and inline rationale turns contracts into arguments.

## Decision

The corpus is data a checker reads, and every rule it states is one the checker enforces. A normative sentence is one addressed clause, and a fact has one home that others reach by `{{id}}`. Build state is computed from pins, never authored. Rationale lives in records under `spec/adr/`; a clause's Why cites one or states a short criterion.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Addressed clauses, computed status, rationale in records *(chosen)* | — | Every new fact needs a registry entry, every performed claim a pin, and every contested direction a record. |
| Prose sections with a citation convention | Detection | Nothing joins a restated fact back to its owner. |
| Status markers written beside each claim | Maintenance | A marker is stale the day the code moves. |
| Rationale inline, marked off by convention | Rot | The argument ends up contradicting the rule. |

## Consequences

- A rename in code reds the gate through its pin, not through a reader noticing.
- A record carries the full argument, so a clause stays within its word limit.
