# 0030 — A pull that cannot converge on a moving index refuses and names the key

**Status:** accepted 2026-09-18
**Decides:** `sync.pull.refusal.digest-mismatch`, `sync.pull.refusal.unconverged-pull`

## Context

A pull fetches the bucket index, diffs its entries against local digests, downloads the
differing objects, and replaces the local catalog last. The ordering is what makes a pull
interrupted partway leave a catalog describing a tree that is still wholly present. The
tree may be behind; it is never inconsistent with what the catalog claims.

The bucket is not frozen while that happens. Another writer pushes, retention deletes run
directories, and snapshot collection removes the objects an older index named. A key the
index listed at the start of the pull can be gone by the time the download reaches it.
This is ordinary, not exceptional — a busy deployment pushes more often than a consumer
pulls.

Recovering from that requires re-fetching the index and retrying the shortfall against the
newer one. Each round can itself be overtaken. The pull is chasing a target that moves,
and whether it catches it depends on the ratio of push rate to download rate, which
nothing in the system bounds.

Separately, every downloaded object carries a digest in the index entry that named it. A
computed digest that differs from that entry means one of two things: the transfer was
truncated, or the object changed between the index read and the download. Both produce
bytes that do not belong in the tree, and the digest is the only signal that distinguishes
a complete transfer from a partial one, since a truncated body can otherwise look like a
smaller file.

## Decision

A downloaded object whose computed digest differs from the entry that named it raises
`SyncObjectDigestMismatch` and the object is discarded rather than written into the tree.
When a key the index named disappears mid-download, the pull re-fetches the index and
retries the shortfall, bounded by the convergence rounds; exhausting them raises
`SyncPullDidNotConverge`, naming the key that kept moving, and leaves the local catalog
untouched. A pull either lands a tree consistent with one index or lands nothing.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Bound the rounds, refuse on exhaustion, leave the catalog untouched** *(chosen)* | The previous consistent state survives every failure. The refusal names the key that kept moving, which points at the writer causing it. | A consumer against a busy bucket sees a refusal rather than a slow success, and the recovery path for a persistently moving index is unspecified. |
| Retry without a bound until the shortfall closes | Every pull eventually succeeds where the bucket eventually quiets. | Lost on liveness. A bucket pushed faster than the re-fetch shrinks the shortfall never finishes, and the pull holds its resources indefinitely with no signal that it will not complete. |
| Accept the shortfall and pull the remainder next time | Progress is monotone, and a busy bucket still converges over several pulls. | Lost on consistency. The catalog would be replaced over an incomplete tree, so it names objects that are not there, and the guarantee that a reader observing the catalog observes a present tree is gone. |
| Assemble the tree from whichever index each object came from | No round is wasted, and every downloaded byte is kept. | Lost on consistency as well, more sharply. A tree assembled from two indexes describes a state that never existed on the writer, and the catalog would name objects from both. |
| Write a mismatched object and repair it on the next pull | A truncated transfer self-heals without a refusal. | Lost on detection. The digest is the one signal separating a truncated transfer from a complete one; writing the bytes anyway spends that signal and leaves a file whose only evidence of being wrong is the check that was ignored. |

## Criteria

1. **What a partially applied pull leaves behind** — whether a failed pull can produce a
   local state that never existed remotely. *This criterion decided it.* The read path
   answers from the tree and the catalog together, so a catalog over an incomplete tree is
   not a stale answer but a wrong one, and it is wrong silently. Every rejected option
   buys throughput or eventual success at the price of that state existing.
2. **Liveness under load** — whether the operation is guaranteed to terminate regardless
   of what the writers do.
3. **Diagnosability** — whether the failure names the thing that caused it. Naming the key
   that kept moving points at a specific writer and a specific tier.
4. **Bytes re-transferred on a failure** — how much work a refused pull discards. This one
   was outranked: a repeated download is expensive and recoverable, while an inconsistent
   tree is cheap to produce and hard to detect.

## Consequences

A consumer's local state is always the tree of exactly one index. A pull that fails
changes nothing, so the previous answerable state stays answerable and a retry starts from
a known position. The digest check also doubles as the transfer's integrity check, so no
separate verification pass is needed.

The accepted cost has two parts. A consumer pulling from a hot bucket may see repeated
refusals rather than a slow success, and every refused round discards the bytes it already
fetched. More importantly, the recovery path for an index that keeps moving faster than
the re-fetch is not specified anywhere: the refusal names the key, and what an operator
does next — quiesce the writer, widen the bound, pull a pinned index — is an open question
carried in this contract's unsettled lines rather than answered here.

Reversing the consistency direction is expensive. Once consumers rely on a catalog never
describing an absent object, relaxing it means every read path has to tolerate a missing
file, which is a change in the read contract rather than in the pull.

## Revisit triggers

- Buckets are observed in which the convergence bound is exhausted under ordinary write
  rates rather than under a pathological one.
- The bucket acquires an index a consumer can pin — a versioned or content-addressed
  top-level entry — so a pull can chase a fixed target and the moving-key case disappears.
- A resumable download path exists that lets a refused round keep its verified objects, so
  the discarded-bytes cost falls enough to reconsider a larger bound.
