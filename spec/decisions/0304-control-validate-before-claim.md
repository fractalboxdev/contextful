# 0304 — Validation precedes the version claim

**Status:** accepted 2026-09-18
**Decides:** `control.apply.refusal.validation`

## Context

An apply turns an edited document into an immutable version. The reconciler derives its
pipeline schedule set from whichever version the store's pointer names, diffs it against
the armed cursor, prunes what a newer snapshot dropped, and keeps the last readable set
running when a poll fails. Arming is therefore not a decision each daemon makes about
whether a version is good; it is the act of adopting whatever the pointer says.

That gives the version store a property worth protecting: every version in it is a
configuration the deployment would actually run. A version that fails validation has no
reader. Each daemon that reaches it refuses it independently, every replica repeats the
same refusal, and the pointer either names something nobody arms or the deployment sits on
its previous set with no marker explaining why. None of those processes can fix the
document, and the operator who can is not watching them.

The document's edits are also not live until an apply, and the claimed version is the
visible marker of which configuration a deployment runs. If a claimed version can be one
nobody runs, that marker stops meaning what it says, and reading the pointer no longer
answers the question an operator asked it.

Validation itself is engine work: it reads the connector schemas, the schedule
specifications, the placement targets and the transform definitions against the rules the
engine enforces at load.

## Decision

An apply validates the document through the engine before claiming anything. A document
failing engine validation raises `ApplyValidationRefused` and claims no version, so the
snapshot store holds no version a daemon would refuse to arm and the pointer names only
configurations the deployment runs.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Validate through the engine, then claim the version** *(chosen)* | Every version in the store is armable, so the pointer is an accurate statement of what runs and a daemon's arming needs no judgement. The operator learns about the fault while they are present to fix it. | The apply path carries a hard dependency on a reachable validating engine, so an operator cannot apply at all while that engine is down — including applies that would have been fine. |
| Claim the version, validate at arming time | The apply path has no engine dependency and always succeeds; faults surface where the configuration actually runs. | Lost on where the fault lands: every daemon refuses the same document independently, the control plane holds a version nobody runs, and the person who can correct it is no longer in the loop. The refusal is repeated N times and actioned zero. |
| Validate in the browser only | Instant feedback while typing; no server round-trip and no engine dependency on apply. | Lost on authority of the check: the verdict then depends on which shell applied and on how current its rules are, so two operators on two clients disagree about the same document and the engine's rules are restated in a second implementation that drifts. |
| Validate in the browser and again at arming, with no apply-time check | Fast feedback plus a real enforcement point. | Lost on the same ground as arming-time validation, and it adds the drift cost of a second implementation without buying an armable-version guarantee. |

## Criteria

1. **Whether the snapshot store can hold a version a daemon refuses to arm** — that is,
   whether the pointer is a reliable statement of what runs. **This criterion decided.**
   The armed set derives from whatever version the pointer names, so a pointer that can name
   an unrunnable version breaks the reconciler's whole basis; the alternatives buy
   availability and feedback speed, neither of which is worth an unreliable pointer.
2. **Where the fault surfaces and who can act on it** — an operator at apply time, or N
   daemons that cannot.
3. **Single authority for the rules** — whether one implementation decides validity, or
   several that can disagree.
4. **Apply availability** — whether an apply can proceed while a dependency is down. This
   points the other way and is the accepted cost.

## Consequences

A daemon's arming path simplifies to adoption: it reads the version the pointer names and
runs it, with no per-daemon verdict and no class of version it has to reject. The audit
record of applied versions is a record of configurations that ran. Client-side checks stay
useful as fast feedback while typing, with no obligation to be authoritative, so they can
be incomplete without being wrong.

The cost accepted: applies are unavailable when the validating engine is unreachable. That
is a real operational coupling — a deployment cannot change its own configuration during an
engine outage, which is one of the moments an operator most wants to. The surface reports
the engine as unreachable rather than offering an unvalidated claim as a fallback, because a
fallback that skips validation reintroduces exactly the version the decision excludes.

## Revisit triggers

- Engine unavailability blocks applies during incidents often enough that the coupling is
  the dominant operator complaint.
- A validation class emerges that cannot be evaluated without the runtime environment of the
  arming host, so apply-time validation is structurally incomplete.
- Validation latency grows far enough that an apply stops feeling like a single action.
