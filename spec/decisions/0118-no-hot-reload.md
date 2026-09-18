# 0118 — A connector version applies to runs admitted after the change, never to a live one

**Status:** accepted 2026-09-18
**Decides:** `connector.package.refusal.live-reresolution`

## Context

A run is journaled. When the engine admits a connector it records the tuple that identifies
the bytes it resolved, and a replayed read resolves its artifact from that record rather
than from the name. That is what makes replay a reconstruction of what happened instead of
a re-execution of what would happen today.

Re-resolving a connector while a run is live breaks the record's meaning from the middle. A
run's early steps ran against one artifact; its later steps would run against another; and
the journal carries one tuple for the whole. Replaying it resolves whichever version the
record names and produces output that disagrees with what the run actually landed. The
disagreement is not a crash. It is a quiet divergence between recorded output and replayed
output, which is exactly the property replay exists to guarantee the absence of.

The pull toward reloading in place is real. An urgent connector fix — a vendor changed a
field, a parse is dropping rows — wants to reach the run that is currently failing, not the
next one. And the engine already has a drain mechanism for world-version changes, which
suggests draining on any version change as a general answer.

But draining every run on any change is expensive for the common case. Most connector
updates are minor-compatible, most runs are long, and a policy that stops in-flight work on
every publish makes publishing something operators schedule around rather than do.

## Decision

Re-resolving or reloading a connector while a journaled run is live raises
`ConnectorHotReload`. A new version applies to runs admitted after the change. In-flight
runs complete against the artifact their record names.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Apply to runs admitted after the change** *(chosen)* | Every journaled run holds one artifact for its whole length, so replay reproduces recorded output. | An urgent connector fix waits for in-flight runs to finish or be cancelled. |
| Reload and re-pin in place | The fix reaches the run that needs it, immediately. | Lost on replay fidelity: a replayed step would resolve different bytes than the ones that produced the recorded output, and the divergence is silent. |
| Drain every run on any version change | One uniform rule; no live run ever holds a stale artifact. | Lost on cost for a minor-compatible artifact: every publish stops in-flight work, which turns routine connector updates into scheduled operations. |
| Record a version change mid-run in the journal and replay each segment against its own artifact | The fix lands live and replay stays faithful. | Lost on complexity against benefit: the run record becomes a sequence of artifact tuples with boundaries, and every consumer of the record — replay, audit, accountability — learns to walk it. |

## Criteria

1. **Whether replay can diverge from recorded output.** *This criterion decided it.* The
   run record is what the accountability and disclosure surfaces read to answer what a run
   did. A record that can be replayed into a different answer than the one it describes is
   not a record. Operational urgency is a recurring cost; a record that cannot be trusted
   is a permanent one.
2. **Cost of a routine connector publish.** Whether an update disturbs in-flight work.
3. **Latency of an urgent fix.** How long a broken connector keeps running.
4. **Complexity carried by every reader of the run record.** How many shapes a record can
   hold.

## Consequences

A run's artifact identity is fixed at admission and stated once in its record. Replay,
audit and the accountability surface each read one tuple. Publishing a connector update is
a non-event for running work.

The accepted cost: an urgent fix waits. An operator who needs a broken connector replaced
immediately cancels the in-flight runs and lets the next admission pick up the new version,
which means the remedy for urgency is discarding work rather than upgrading it. For long
runs — a large backfill, a wide expansion walk — that can be a substantial amount of
progress thrown away, and the engine offers no partial-resume across a version change.

How long runs actually are in practice, and therefore how often this cost is felt, is not
established.

## Revisit triggers

- Long-running backfills make the cancel-and-restart remedy expensive enough that operators
  are observed avoiding connector updates rather than taking them.
- The run record gains a segmented artifact history for another reason, which would make
  the mid-run version change cheap to represent faithfully.
- A class of connector change is identified that provably cannot affect a read's output,
  which would let that class apply live without touching replay fidelity.
