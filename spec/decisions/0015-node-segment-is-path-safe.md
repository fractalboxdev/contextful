# 0015 — A resolved node id is validated against a path-safe pattern before a directory is created

**Status:** accepted 2026-09-18
**Decides:** `store.lay-out.refusal.unsafe-node-segment`

## Context

Two machines can write parts of one logical run. The store keeps them apart by giving each
its own directory segment: run output lands at `data/runs/<run-id>/<node-id>/`, so two
writers produce two `part-00000.parquet` files that do not collide on one name. The same
node id appears a second time in the run's request ledger, whose path is
`requests/<run-id>.<node-id>.parquet` — there the value is not a directory component but a
field inside a filename, separated from the run id by a dot.

The node id is resolved from the deployment: a hostname, a configured identifier, a
container's name. Those values are not drawn from a safe alphabet. A slash turns one
segment into two directories, a dot sequence walks upward, a space survives a filename and
breaks a shell-quoted operator command, and an empty value resolves to the parent
directory itself.

The two interpolation sites are what make escaping insufficient rather than merely ugly.
A path segment and a dot-delimited filename field have different escaping rules — a dot is
harmless in a directory name and ambiguous inside the ledger filename, and a percent-escape
that is correct in one site is a literal in the other. One resolved value would then render
two ways on disk, and the merge arm that decides which parts a given node owns stops
matching its own ledger.

There is also a silent-rename failure. If an unsafe value is replaced with a generated
substitute, the machine writes under a name nobody configured, and across restarts one
physical writer accumulates output under two names — its run tier splits, and every
per-node accounting reads as two machines.

## Decision

A resolved node id is matched against `^[A-Za-z0-9._-]{1,64}$` before any directory is
created. A value outside that pattern raises `StoreUnsafeNodeSegment` and the store does
not start writing. The one accepted value is interpolated verbatim into the run path
segment and into the request-ledger filename, so both sites read the same bytes.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse an unsafe value at resolution** *(chosen)* | One value renders identically at both interpolation sites; nothing unrecoverable is written | A deployment whose node identifier carries a slash or a space refuses to start until the identifier is changed |
| Percent-escape the value into the path | Every configured identifier is accepted, whatever its bytes | Lost on shared interpolation: the ledger filename and the path segment escape differently, so one value renders two ways and the merge's ownership arm stops matching its own ledger |
| Fall back to a generated id on an unsafe value | The deployment always starts | Lost on recoverability: a machine that silently renames itself splits its own run tier across two names, and the split is only visible as an accounting anomaly long after the rows land |
| Validate at the first write instead of at resolution | Startup never fails on configuration alone | Lost on where the value becomes unrecoverable: a run has already been accepted, and the refusal lands mid-pull rather than at the moment the identifier is read |

## Criteria

1. **Shared interpolation** — whether one resolved value is consumed by two sites that
   would escape it differently. *(the one that decided it)* The run path segment and the
   ledger filename both consume this value; escaping one does not escape the other, and a
   value with two renderings breaks the ownership match between parts and ledger. That
   outranks acceptance breadth, because a rejected identifier is a one-line configuration
   fix and a split rendering is a data-association bug found much later.
2. **Where a bad value becomes unrecoverable** — how much has been written by the time the
   problem is detectable.
3. **Identity stability** — whether a writer keeps one name across restarts.
4. **Operator legibility** — whether the on-disk name is the name in the configuration.

## Consequences

Every path the store builds from a node id is safe by construction, and an operator reading
a run directory sees the identifier they configured. The merge that decides which parts a
node owns compares the segment against the ledger filename field directly, with no
unescaping step on either side.

The cost accepted is a startup refusal: a deployment whose node identifier carries a slash,
a space, a non-ASCII character or more than 64 bytes does not run until the identifier is
changed. That includes identifiers inherited from an orchestrator's naming scheme, where
the fix is out of the deployment's hands and is felt as friction on adoption.

The 64-byte ceiling and the ASCII-only alphabet are now a compatibility surface. Widening
either later is safe for new stores and changes nothing for existing ones, but narrowing
either refuses a store that already writes.

## Revisit triggers

- A deployment environment in common use assigns node identifiers that cannot be made to
  match the pattern without operator intervention at every node.
- The request ledger stops being addressed by filename fields, removing the second
  interpolation site and with it the reason escaping was insufficient.
- Node identifiers routinely exceed 64 bytes in a real fleet, making the length bound
  rather than the alphabet the refusing arm.
