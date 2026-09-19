# D48 — The bucket index commits by compare-and-set over a scoped union of the remote

**Status:** accepted

## Context

The bucket index is the one object a consumer reads to learn what a deployment holds, and writing it is a push's commit point. Several writers share it, each seeing a partial view. Deletion is real — retention drops run directories, collection removes superseded snapshots — so a blanket union resurrects every removed entry.

## Decision

`store.merge` commits by compare-and-set over a scoped union: the remote contributes an entry only where this writer has never written. An owned entry absent locally drops out, which is how retention and collection reach a consumer. Ownership is readable from the key: run directories carry a node segment, request-ledger files carry the node in the filename. A lost race re-reads, re-merges and re-commits within the retry bound; exhaustion raises `SyncManifestRebaseExhausted`, reports every object already uploaded, and asks for a re-run.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Compare-and-set over a scoped union, bounded rebase *(chosen)* | — | Every writer-owned tier needs an ownership arm; a key shape falling to the unowned default never propagates deletion. |
| A blanket union of both indexes | Deletion propagation | The index would grow forever against a shrinking tree. |
| Replace the remote index wholesale | Visibility of committed objects | A race loser would silently unlist the winner's objects. |
| Replicated conflict-free types on the data plane | Necessity | The append-only layout already prevents most conflicts; the machinery would cost on every push. |
| Unbounded rebase retries | Termination | A writer losing every race would hold its process indefinitely for nothing a re-run lacks. |

## Consequences

- A consumer's view shrinks when the producer's tree shrinks.
- Adding a tier is a merge change, not only a layout change.
- A contended bucket can leave a writer's objects durable and unlisted, visible in the refusal.

## Revisit

- A key shape arrives whose ownership is not derivable from the key.
- Retries exhaust under ordinary concurrency.
- The index outgrows a single-object commit.
