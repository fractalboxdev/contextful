# 0309 — The console is a caller over a store and holds no privileged path under its tables

**Status:** accepted 2026-09-18
**Decides:** `console.ground.refusal.direct-file-read`

## Context

The console holds no data, no reader and no path under the tables. Everything it shows
arrives as the result of a governed read: the engine answers each call under the caller's
grants, and enforcement rewrites what comes back per row and per column — a predicate
narrows which rows a caller sees, a mask blanks a column's values for callers without reach
to it.

Two features on this surface look like they want to bypass that. The data-file gallery lists
committed files with the run that wrote each and its size. The file preview shows a single
file's rows. A committed file is Parquet sitting in object storage, and a table function
resolving a path straight against those bytes would answer both features in one range read —
faster, simpler, and with no query planning at all.

That path reads the file as stored. The rows in it are the rows before enforcement, because
enforcement is a rewrite the engine applies when it serves a relation, not a property of the
bytes on disk. A preview built that way shows the reader a row the same reader's query would
have filtered out, and a column value their query would have masked. The gallery boundary
can check whether the reader may see the file at all, which is a per-file decision; it cannot
express a per-row predicate or a per-column mask, because those are not questions about a
file.

The surface is not the owner of the store. It presents a capability and receives what the
store decides to return.

## Decision

The console reads its files back through the enforced relation. A table function resolving a
path straight against stored bytes raises `ConsoleFileAccessDirect` on this surface. The
file preview returns its rows through the same enforced relation an ordinary query reads,
and both file tools answer under the reader's grants like any other call.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Read files back through the enforced relation; refuse a direct path read** *(chosen)* | One enforcement path covers every surface, so a rule tightened in the engine tightens the preview at the same moment with no second change. A preview cannot show what a query would hide. | A preview of a large committed file costs a query rather than a byte range, and a file that no longer maps to a table cannot be shown at all. |
| Direct file reads, restricted by a grant check at the gallery boundary | Cheap reads; one authorization check at an obvious place. | Lost on leakage: a row predicate and a column mask are per-row rewrites, and the gallery boundary decides per file. The check answers a different question from the one that keeps the rows safe, so it passes exactly when the leak occurs. |
| A separate preview-only enforcement path reimplementing the rewrites | Keeps the fast read and the correct rows. | Lost on single-path coverage: two implementations of one rule diverge, and the divergence is invisible until it leaks — the preview is not where anyone looks when a policy is changed. |
| Materialize an enforced copy of each file per reader for preview | Fast reads with correct rows, precomputed. | Lost on the same coverage ground once the copy goes stale, and it multiplies stored bytes by the number of distinct grant shapes, which is unbounded. |

## Criteria

1. **Whether a preview can show a row a query would mask** — leakage through a second read
   path. **This criterion decided.** Enforcement that holds on one path and not another is
   not enforcement, and the gallery's per-file check cannot express the per-row and
   per-column rewrites that carry the guarantee.
2. **Single enforcement path across every surface** — whether one implementation decides what
   a caller sees, or several that can drift.
3. **Read cost for a large file** — a planned query against a byte range. This points the
   other way.
4. **Implementation cost of rebuilding both features as tools** — real, one-time, and the
   weakest of the four.

## Consequences

Enforcement changes propagate to every surface at once, and reviewing what a reader can see
means reading the engine's rules rather than auditing each feature. The file tools become
ordinary calls in the admitted set, so they inherit the vantage, the grants and the audit
record every other call carries, with no special case. The console's claim to hold no
privileged path under the tables becomes true without exception, which is what makes it
safe to expose the surface to a reader whose grants are narrow.

The cost accepted: previewing a large committed file costs a planned query rather than a
byte range, so the feature is slower and scales with the file rather than with the page.
A file that no longer maps to a table — one written by a run whose table was dropped — cannot
be previewed at all, which is a real gap in a forensic use of the gallery, and the surface
shows it as a file with no preview rather than inventing a path to its bytes.

## Revisit triggers

- Preview latency on ordinary committed files becomes the dominant complaint about the
  gallery.
- Enforcement gains a form that can be evaluated against stored bytes directly, which would
  remove the reason the direct path leaks.
- Forensic access to files that no longer map to a table becomes a stated requirement, which
  this decision cannot serve and a different, non-visitor surface would have to.
