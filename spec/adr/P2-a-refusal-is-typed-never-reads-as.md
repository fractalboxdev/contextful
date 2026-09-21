# P2 — A refusal is typed, never reads as an empty result, and echoes only caller input

**Status:** accepted

## Context

An empty list answers two questions at once: nothing exists, or the caller may not see it. A caller cannot branch on that answer. A refusal naming what it found in storage turns every held identifier into an oracle for what lies beyond the grant.

## Decision

A refusal is a typed error a caller branches on, one per condition. Zero rows, an empty list and an empty trace mean only that the answer is empty. A refusal echoes what the caller supplied and nothing read from storage.

- `authority.filter-rows` answers an out-of-scope read on a granted table with `scope_denied`, HTTP 403, naming table, granted scope and requested scope; a table with no grant reads as a name nothing is bound to.
- `run.record` refuses an uncovered pipeline by name and a run trace without naming the pipeline it resolved; an unknown resume token answers 404 and allocates nothing.
- `disclosure.explain` returns a decision and its path, never a row, names groups and not members, and prints an assurance verdict beneath its coverage.
- `surface.dispatch` judges a tool-protocol step on the result envelope, not on transport status.
- `surface.ground` answers a turn with no tool result by saying the store holds nothing, never with model-composed prose.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Typed refusal per condition, echoing caller input *(chosen)* | — | Callers handle more error variants, and each surface maps each onto its wire status. |
| Answer a denial with an empty result | Distinguishability | Denied, absent and quiet read the same, and every aggregate reports a correct zero over the wrong question. |
| A generic refusal naming nothing | Actionability | The caller cannot tell a typo from a missing grant. |
| Echo stored values or resolved names | Non-disclosure | A held run id or a tenant guess reveals what stands behind it. |
| Trust the transport status | Where the failure surfaces | A protocol error inside a 200 records as success. |

## Consequences

- Ungranted and out-of-scope answer differently: one hides existence, the other names the scope the caller already holds.
- Every refusal carries a stable identifier a test pins, so a refusal that stops firing reds the gate.
- An empty store reads as empty rather than as a model's guess.
