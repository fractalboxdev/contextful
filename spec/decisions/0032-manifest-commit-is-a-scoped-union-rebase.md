# 0032 — The bucket index commits by compare-and-set over a scoped union of the remote

**Status:** accepted 2026-09-18
**Decides:** `sync.merge.refusal.retries-exhausted`

## Context

The bucket index is the one object a consumer reads to learn what a deployment holds. It
is a pure function of the file tree beneath the prefix, and writing it is the commit point
of a push: every key it names has already landed durably, and a push interrupted before
that write leaves the previous index intact.

One index, more than one writer. Each writer walks its own store, which is a partial view
of the bucket — it holds its own run directories, its own request ledger files, its own
snapshots for the tables it folds, and nothing another machine wrote. An index computed
from that walk alone describes a strict subset of what the bucket contains.

So the commit cannot be a replacement, and the writer has to combine its view with the
remote's before committing. The obvious combination is a union, and the obvious union is
wrong, because deletion is a real operation here: retention drops run directories and
snapshot collection removes superseded snapshots, both locally, and both have to reach a
consumer through the index. A blanket union cannot express an absence — an entry the
writer deleted comes straight back from the remote copy.

The distinguishing fact is ownership. Where a writer has written before, its local view is
authoritative, including about what is no longer there. Where it has never written, it
knows nothing and the remote's entry stands. That split is expressible only if every
writer-owned tier is identifiable from the key alone, which is why run directories carry
a node segment and request-ledger files carry the node in the filename.

## Decision

The index commit is a compare-and-set over a merge of the remote index, and the merge is a
scoped union: the remote contributes an entry where this writer has never written, and
nowhere else. An entry this writer owns and no longer holds locally drops out, which is
how snapshot garbage collection and run retention reach a consumer. A writer losing the
race re-reads, re-merges and re-commits, bounded by the commit retries; exhausting them
raises `SyncManifestRebaseExhausted`, reports that every uploaded object is already in the
bucket, and asks for a re-run rather than committing an index it did not rebase. The merge
runs under both coordination modes — the compare-and-set is what makes a concurrent commit
safe, not what makes the union correct.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Compare-and-set over a scoped union, bounded rebase, refuse on exhaustion** *(chosen)* | Deletion propagates, concurrent commits serialize, and a lost race costs a re-read rather than a writer's objects. | Every writer-owned tier needs its own ownership arm. A key shape falling through to the unowned default is one whose deletions never reach a consumer. |
| Replace the remote index wholesale | Simplest possible commit, and the index is exactly a function of one writer's tree. | Lost on data visibility. The loser of a race erases the winner's index of objects still present in the bucket, and nothing errors — the objects survive unlisted, which is the same shape as a prefix collision. |
| A blanket union of both indexes | No ownership rules, no key-shape analysis, and no writer can hide another's objects. | Lost on deletion. Removed directories are resurrected in the index forever, so retention and snapshot collection never reach a consumer and the index grows without bound against a shrinking tree. |
| Conflict-free replicated types on the data plane | Concurrent writes merge by construction, with no retry loop and no ownership arms. | Lost on necessity. The data plane is append-only and the metadata that is not — cursors, snapshot pointers — has a defensible per-class rule already. The machinery would be paid for continuously to solve conflicts the layout mostly prevents. |
| Retry the rebase without a bound | A hot bucket eventually commits rather than asking for a re-run. | Lost on liveness. A writer losing every race holds its process indefinitely, and the uploaded objects are already durable, so waiting buys nothing a re-run does not. |

## Criteria

1. **Whether deletion can propagate** — whether the merge can express that an entry is
   gone. *This criterion decided it.* Retention and snapshot collection are routine
   operations, not edge cases, and an index that cannot express their effect diverges from
   the tree permanently in the only direction a consumer cannot detect. Simplicity of the
   merge and freedom from ownership rules are both genuine advantages of the blanket
   union, and neither survives an index that grows forever.
2. **Whether a concurrent commit can hide committed objects** — whether a race costs
   visibility of data that is present.
3. **Termination** — whether the commit path is bounded regardless of the other writers.
4. **Ongoing cost of the mechanism** — what the system pays per push for the conflict
   story it buys.

## Consequences

Deletion reaches a consumer, so retention and snapshot collection are real: a consumer's
view shrinks when the producer's tree shrinks. A losing writer never erases a winner's
entries, and a push that cannot commit leaves the previous index in place while reporting
that its objects are already uploaded, so a re-run is cheap.

The accepted cost is that the ownership arms are not optional and not automatic. Every
writer-owned tier has to carry an arm keyed on something readable from the key, and a new
key shape that falls through to the unowned default is one whose local deletions never
reach any consumer — silently, since the entry simply persists. The request ledger spells
its node in the filename rather than in a path segment precisely to force that arm to
exist. Adding a tier is therefore a merge change rather than a layout change.

Exhausted retries ask for a re-run, which means a sufficiently contended bucket can leave
a writer unable to publish while its objects sit durable and unlisted. That state is
visible in the refusal, not in the index.

## Revisit triggers

- A new key shape is introduced whose ownership is not derivable from the key, which would
  force either a manifest-side ownership record or a different merge rule.
- Commit retries are observed to exhaust under ordinary write concurrency rather than
  under a pathological burst.
- The index outgrows single-object commit for a large store, so the commit point moves to
  a sharded or incremental structure and the union has to be re-derived over it.
