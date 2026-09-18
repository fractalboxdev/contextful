# 0303 — A losing apply reloads the newer version and reapplies onto it

**Status:** accepted 2026-09-18
**Decides:** `control.apply.refusal.version-race`

## Context

An apply validates the edited document, claims an immutable `<store>/manifest@v{N}.toml`,
and advances that store's pointer by compare-and-swap. The version counter is per store.
Once claimed, a version is immutable and stays readable at the key that named it; the owner
collects no superseded version, because retention is operator policy.

Two operators can apply against one store at the same time. The editable document is a CRDT
and merges their concurrent typing, but an apply is not an edit — it is a claim on a number
and a swap of a pointer that other processes watch. The reconciler derives its schedule set
from whichever version the pointer names, so the swap is the moment a deployment's behavior
changes.

That makes the race consequential in two directions. If a claimed version can be
overwritten, an immutable artifact is not immutable, and a consumer that armed itself
against version N can find N's contents changed underneath it. If the pointer can move
backwards — advanced to a version whose author never read the one before it — then a
deployment runs a configuration that silently discards an applied change, and the operator
who made that change sees it disappear with no event marking the loss.

The editable document already has the machinery for the merge case: it persists under
conditional replacement and concurrent edits reload and reapply on contention. The apply
path needs an answer of the same shape at the version boundary.

## Decision

An apply whose compare-and-swap loses raises `ManifestVersionConflict`, reloads the newer
version, and reapplies the pending edits onto it. No applied version is overwritten and the
pointer never moves to a version that did not derive from the one before it. The conflict
surfaces to the operator at that moment, carrying whatever the reapplication could not
reconcile.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Compare-and-swap; a loser reloads the newer version and reapplies onto it** *(chosen)* | Every claimed version is immutable and every version derives from its predecessor, so the pointer's history is a chain rather than a set of overwrites. A losing operator keeps their edits. | The operator whose apply lost has their edits reapplied onto a document they never read, and a genuine disagreement between two operators becomes visible only at the apply, not while they were typing. |
| Last-write-wins on the pointer | Simplest possible apply; no conflict path to implement or explain. | Lost on immutability: an applied version is superseded by one that never saw it, so a consumer can arm a configuration that was rolled back under it with no event to observe. |
| Hold a lock on the document for the duration of an apply | Conflicts never arise; each apply sees a stable predecessor. | Lost on availability: a stuck apply — a slow validation, a dropped browser session — blocks every other operator on that store, and the lock needs a lease, an expiry and a recovery path of its own, which is more moving parts than the retry it replaces. |
| Queue applies and serialize them server-side | No conflict surfaces to the operator; ordering is total. | Lost on the same immutability question one step later: an apply dequeued after another still carries edits made against an older document, so either it silently overwrites or it hits the identical reapplication problem, now with the operator no longer present to resolve it. |

## Criteria

1. **Immutability of a claimed version** — whether an artifact other processes consume can
   change after they have consumed it. **This criterion decided.** The reconciler, the
   audit record and the retained history all treat a version as a fixed fact, and
   last-write-wins makes that treatment wrong; nothing the other options buy is worth a
   version that is only conditionally immutable.
2. **Survival of a losing operator's work** — whether the edits are kept or silently
   discarded.
3. **Availability under a stalled participant** — whether one hung session can block others.
4. **Visibility of a real disagreement** — whether two operators changing the same field
   learn about each other, and when.

## Consequences

The version chain becomes a readable history: each applied version derives from the one
before it, and an audit reading the sequence sees no gaps and no rewrites. A consumer can
cache a version's contents by its key indefinitely. The retry is bounded by the number of
concurrent appliers rather than by a lease timer.

The cost accepted: a losing operator's edits land on a document they did not read. The
reapplication is mechanical, so a field both operators changed resolves by the document's
own merge rules rather than by either operator's intent, and that is precisely when the
conflict surfaces — after the fact, at the moment of the second apply, rather than while
the two were editing. A deployment with many concurrent operators on one store will see
this regularly.

Reversing toward last-write-wins is cheap in code and expensive in trust, since every
consumer that has cached a version by key would need to stop.

## Revisit triggers

- Apply conflicts on a single store become frequent enough that reapplication regularly
  produces a document neither operator intended.
- A deployment needs applies to be totally ordered across stores rather than per store,
  which changes what the pointer is.
- Live presence between concurrent editors makes disagreement visible during editing, so
  the apply-time conflict stops being the first signal.
