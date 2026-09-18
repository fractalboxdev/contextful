# 0154 — An unreachable engine ends the run, and a link engine never reports one

**Status:** accepted 2026-09-18
**Decides:** `derive.bind.refusal.link-reader-unavailable`

## Context

The unit loop is serial and every unit has an attempt budget — three by default. A unit that
fails settles or retries; a unit that exhausts its attempts is retired with a marker row, and
the anti-join never brings it back. Attempt budget is therefore a finite, non-renewable
resource spent per row.

The two drivers put the word "engine" against two different things, and the difference is what
this decides.

Under `exec`, the engine is one resolved binary on this machine, pinned by digest, the same for
every unit in the run. If it is missing, not executable, or its interpreter is gone, that is
one fact about the machine. Discovering it per unit would mark hundreds of rows failed against
a single absent file, spend every one of their attempts, and fill the derived table with
markers that say nothing about the rows they are attached to. Recovering from that costs a
human deleting marker rows.

Under `fetch`, the engine names a different publisher for every unit. The address came out of
the row; the host is whoever that publisher chose. A host that does not resolve, refuses the
connection or times out is a fact about that one row's destination. Aborting the run on it
would let one hostile or dead publisher stop work on every other unit in the list — and pre-
socket guards already establish the opposite principle for this driver: one hostile publisher
costs one row.

So the same error variant means "stop" in one position and "this row" in the other.

## Decision

`EngineUnavailable` describes the engine rather than one unit: it returns out of the read and
ends the run, leaving the outstanding units outstanding and spending none of their attempts.
The next tick finds them and retries whole.

A `LinkReader` returning `EngineUnavailable` raises `DeriveLinkEngineUnavailable`. Its engine
is a different publisher per unit, so one dead name costs one row, and a transport failure
there settles or retries that unit through the ordinary error path.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Run-scoped variant, forbidden to the link anchor** *(chosen)* | An unreachable binary costs nothing and retries whole; a dead publisher costs one row | The obligation is trait-level and a compiler cannot check it — a link adapter that returns the variant takes down batches it should not |
| Failing units one at a time on an engine-level failure | One rule for both drivers, no trait-level obligation | Lost for the subprocess path: one unreachable binary marks hundreds of units failed, spends every attempt budget in the table, and leaves marker rows a human deletes |
| Aborting the run on any transport failure | One rule, maximally cautious | Lost for the link path: the engine there is a different publisher per unit, so one dead name stops every unrelated row in the list |
| A per-unit variant and a per-run variant as separate error cases | Both meanings expressible with no trait-level rule | Lost on adapter judgement: the adapter would classify each failure, and a vendor client cannot distinguish "this host is down" from "our whole API is down" from one refused connection |
| Making run-scoping a property the driver declares rather than the anchor | One declaration, checkable at build | Lost on precision: the property follows from whether the engine names one thing or a different thing per unit, which is a property of the anchor and not of the mechanism |

## Criteria

1. **Whether the engine names one thing or a different thing per unit** — whether an
   unreachability report is a fact about the run or about the row. **This criterion decided
   it, alone.** The scope of the failure is determined by the scope of the thing that failed;
   any rule that assigns one scope to both anchors is wrong for one of them on every run.
2. **Attempt budget spent on a fact unrelated to the row** — how many rows a single
   machine-level fault retires.
3. **Blast radius of one hostile or dead publisher** — how much unrelated work one bad address
   stops.
4. **Whether the rule is machine-checkable** — whether a compiler or the build can enforce it.

Criterion 4 is where this decision is weakest and it lost to criterion 1: a checkable rule
that assigns the wrong scope to one anchor is worse than an unchecked rule that assigns the
right one to both.

## Consequences

The obligation on `LinkReader` is stated and not compiled. A link adapter that returns
`EngineUnavailable` ends runs it has no business ending, and the refusal catches it at the
point the value is returned rather than at build. That is the cost accepted.

What gets easier: an operator fixing a missing binary loses nothing. The units that were
outstanding when the run ended are still outstanding, still hold their full attempt budget,
and are derived on the next tick with no marker rows to clean up.

What is now expensive to reverse: attempt accounting on marker rows already written reflects
this scoping, so moving to per-unit failure for exec engines would make historical retirement
counts incomparable with new ones.

## Revisit triggers

- The anchor traits gain a way to express the scoping in types, so the link obligation becomes
  a compile-time property rather than a runtime refusal.
- An `exec` engine class arrives whose binary can differ per unit, so the subprocess engine
  stops naming one thing for the whole run.
