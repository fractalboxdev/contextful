# 0189 — Narrowing is checked in the deriving process and again at admission, and a widening child names the dimension that widened

**Status:** accepted 2026-09-18
**Decides:** `authority.attenuate.refusal.widening`

## Context

A holder derives a narrower child with no round trip to the issuer. Derivation is a
local signature over the parent's transmitted bytes together with an appended block —
which means the deriving party is free to append whatever it likes, and the question of
whether the result is actually narrower is answered by whoever reads the chain, not by
whoever wrote it.

Narrowing has several dimensions that move independently: actions, table patterns,
tenant scope, aggregate constraints, the template allowlist, and the validity window.
A child can narrow on four of them and widen on the fifth. Composition compounds this —
a grandchild narrows against the child it came from, so a widening laundered through an
intermediate hop would be invisible to anyone examining only the last block.

Two parties can perform the comparison. The deriving holder can run it before it signs,
where a failure is cheap, local and lands on the code that caused it. The checkpoint can
run it at admission, where it is the only check that carries weight, since the deriving
holder is precisely the party whose honesty is in question.

## Decision

The same narrowing comparison runs in the deriving process and again at the checkpoint,
over the whole chain rather than its last hop. A proposed child broader than its parent
on any dimension raises `AttenuationWidens` and names the dimension that widened.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The identical comparison at derivation and at admission, over the whole chain** *(chosen)* | An illegal child fails inside the deriving process, where the cause is; and an illegal child that reaches a checkpoint anyway is refused there, where the guarantee lives. | Admission pays chain-depth comparison work on every request, and one comparison is implemented once but executed in two very different environments. |
| Check at admission alone | Minimal work; the check lives where it counts. | Loses on time to discovery: an illegal child fails at its first request, in another process, in another party's logs, with the deriving code long finished and no signal reaching it. |
| Check at derivation alone | Cheapest possible admission. | Loses outright on trust: the deriving holder is the party under question, so a check it performs on itself is not evidence of anything a checkpoint can rely on. |
| Route derivation through the issuer, which validates centrally | One authoritative check, and a record of every child. | Loses on offline derivation: a round trip per child is unaffordable at per-query grain, and the whole derivation model exists to keep the issuer off the read path. |
| Check the last hop alone at admission | Constant-time admission regardless of depth. | Loses on composition: a widening at an intermediate hop is never examined, so a grandchild that narrows against a widened parent admits to reach its grandparent never held. |
| Refuse without naming the widened dimension | Nothing disclosed about the parent. | Loses on diagnosability, and discloses nothing worth protecting — the deriving holder already holds the parent it is narrowing from. |

## Criteria

1. **Whether the check is evidence** — whether the party performing it has an interest
   in its outcome. *This criterion decides.* A derivation-side check is useful and is
   not evidence; no amount of care in the deriving process substitutes for the
   checkpoint's own comparison, so the admission-side check is not optional at any cost.
2. **Time to discovery** — how close the failure lands to its cause. This is the whole
   argument for the second, non-authoritative check.
3. **Per-request cost** — work paid at admission, growing with chain depth.
4. **Diagnosability of the refusal** — whether the message names the dimension.

## Consequences

Deriving code gets a real signal: a mistake in how a child is constructed fails in the
process that constructed it, naming the dimension, rather than surfacing as an
unexplained rejection at a consumer's first request.

Admission cost now grows with delegation depth, and the size of that cost at the deepest
chains the profile supports is unmeasured — the supported depth is itself still open, so
the per-request work at maximum depth is unknown rather than bounded. On the shallow
chains in use, the comparison is a structural walk over a small number of dimensions.

The accepted cost is doing the work twice, plus the requirement that the two executions
stay identical. A divergence between the deriving check and the admission check is a
class of bug with an unpleasant signature — children that pass locally and fail
remotely, or worse, the reverse.

Reversing toward a last-hop-only admission check is expensive because composition is
what holders rely on: an agent fanning work out to sub-agents assumes a grandchild
cannot out-reach the agent itself.

## Revisit triggers

- Verification cost at maximum delegation depth is measured and lands high enough to
  show in read latency at the tail.
- The profile fixes a maximum chain depth, which turns the unbounded cost into a bounded
  one and changes what the per-request criterion is worth.
- The two implementations of the comparison diverge in practice, which would argue for
  one shared artifact rather than one shared rule.
