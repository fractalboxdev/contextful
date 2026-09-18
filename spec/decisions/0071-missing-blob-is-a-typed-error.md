# 0071 — An unresolvable blob reference fails with a typed error carrying the reference

**Status:** accepted 2026-09-18
**Decides:** `run.journal.refusal.missing-blob`

## Context

A journaled step runs its effect exactly once: the first call under a key enters the effect
and durably writes what it produced, and every later call under that key returns the written
value without entering the effect again. Values of 1 MiB or smaller live inline in the
journal row. Above that cutoff the value lands in a content-addressed file named by its
sha256 under the blob directory, and the row holds that reference. A caller receives bytes
either way and branches on neither tier.

The row and the file have different lifetimes. The row lives in the catalog; the file lives
on the filesystem, where a partially restored backup, a pruned blob directory, a filesystem
error or a catalog restored past the files can each leave a reference resolving to nothing.
The reference is a hash, so nothing can be reconstructed from it — the recorded value is
gone and no retry recovers it.

What the read does at that moment decides what every caller downstream can tell. A journaled
read sits in the middle of a run body, and the value it returns flows into the land path,
into a cursor commit, into whatever the body computes next. A run that resolves a recorded
batch as empty lands zero rows, commits a position past them, and closes successfully — the
land path has no way to distinguish that from a pull that legitimately produced nothing,
because those two cases are byte-identical at the point of the read. The wrong result is
durable, committed and unmarked.

A panic distinguishes the case but takes the process rather than the run, so a single lost
file stops every other run sharing that process, and the operator reads a stack rather than
the reference that went missing.

## Decision

A row holding a blob reference whose file resolves to nothing fails with `BlobMissing`
carrying the reference — never a panic, never an empty value standing in for the recorded
one. The failure is a typed error a caller branches on and a run's retry classification
reads, and the reference it carries is the content hash an operator searches the blob
directory for.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A typed `BlobMissing` carrying the reference** *(chosen)* | A caller downstream can distinguish a lost recorded value from every other outcome; the failure is contained to the run; the operator gets the hash to look for | Every caller of a journaled read carries one more arm, and the reference appears in an error message an operator reads |
| Panic | Loud, unmissable, impossible to ignore at any call site | Lost on containment: it takes the process rather than the run, so one lost file stops unrelated runs, and the operator reads a stack instead of the reference |
| Return an empty value | No call-site change at all; the body proceeds as written | Lost on distinguishability: an empty value is indistinguishable from a step that legitimately produced nothing, and the run lands a wrong result the land path accepts and commits |
| Re-enter the effect on a missing blob | The value comes back; the run continues rather than failing | Lost on the exactly-once property: the effect already ran, so re-entering it repeats a vendor call the journal exists to prevent, and the replacement value may differ from the recorded one |

## Criteria

1. **Distinguishability downstream** — what a caller after the failure can tell about what
   happened.
2. **Containment** — how much stops when one file is lost.
3. **Operator legibility** — whether the failure names the thing to go looking for.
4. **Call-site burden** — arms every caller carries.

Criterion 1 decided it. Containment alone would already reject the panic, but it is
distinguishability that rejects the empty value, and the empty value is the dangerous option
precisely because it costs nothing at any call site: it converts a storage failure into a
successful run with a wrong result, committed. A failure a caller can see is worth more than
a call site that stays short, so criterion 4 loses.

## Consequences

A lost blob fails the one run that needed it, with the hash in the message, and every other
run in the process continues.

Every caller of a journaled read carries one more arm. Most pass it through, which is the
recurring cost accepted here.

The reference appears in an operator-facing error message. It is a content hash and names
nothing about the data it addressed, but it is one more identifier in a log.

A run that hits this error cannot be completed by retrying: the recorded value is gone and
re-entering the effect is refused, so the run fails terminally and the operator's recovery
is to restore the blob directory or to start a new run. That is the recovery model this
decision accepts, and it follows from the exactly-once property rather than from this rule.

## Revisit triggers

- A blob store with its own durability guarantee relative to the catalog, which would make
  the dangling reference a storage-layer failure rather than a run-path one.
- A recorded-value integrity check at commit time, which would catch the loss before a
  replay reaches the read.
- `BlobMissing` occurring often enough in a deployment that operators want an automated
  recovery path rather than a terminal failure.
