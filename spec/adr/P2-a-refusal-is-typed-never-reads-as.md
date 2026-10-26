# P2 — A refusal is typed, never reads as an empty result, and echoes only caller input

**Status:** accepted

## Context

An empty list answers two questions at once: nothing exists, or the caller may not see it. A refusal naming stored values turns every held identifier into an oracle.

## Decision

A refusal is a typed error a caller branches on, one per condition. Zero rows, an empty list and an empty trace mean only that the answer is empty. A refusal echoes what the caller supplied and nothing read from storage.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Typed refusal per condition, echoing caller input *(chosen)* | — | Callers handle more error variants, and each surface maps each onto its wire status. |
| Answer a denial with an empty result | Distinguishability | Denied, absent and quiet read the same. |
| Echo stored values or resolved names | Non-disclosure | A held run id or a tenant guess reveals what stands behind it. |

## Consequences

- Ungranted and out-of-scope answer differently: one hides existence, the other names the scope the caller already holds.
- Every refusal carries a stable identifier a test pins, so a refusal that stops firing reds the gate.
