# 0002 — Build state is computed from pins that resolve against the tree, and every argument lives in a record

**Status:** accepted 2026-09-18
**Decides:** `corpus.rationale.invariant.argument-lives-in-a-record`, `corpus.rationale.refusal.argument-in-a-contract-file`, `corpus.rationale.invariant.refusal-cites-a-record`, `corpus.rationale.shape.record-anatomy`, `corpus.rationale.refusal.orphan-record`, `corpus.rationale.shape.unsettled-line`, `corpus.rationale.refusal.appendix-heading`, `corpus.state.invariant.status-is-computed`, `corpus.state.shape.pin`, `corpus.state.invariant.unpinned-is-committed`, `corpus.state.invariant.performed-needs-resolution`, `corpus.state.refusal.broken-pin`

## Context

A corpus written before the code says two different things at once: what the tree
demonstrates, and what the design commits the system to doing. A reader who cannot tell which is
which reads a plan as a description, and an author who marks the difference by hand marks
it correctly on the day of writing and never again. The frequent event in a live tree is a
rename — an error variant changes spelling, a command verb moves, a schema key is
adjusted — and a rename is exactly the event a hand-written marker cannot notice.

The corpus also attracts argument. A sentence explaining why a refusal points the way it
does reads as helpful in the file where the refusal lives, and it is the thing that rots
first: the alternatives it dismisses were the alternatives available at the time, the
criteria it names are the criteria that mattered then, and a later reader cannot tell
which parts of it still hold. Worse, an argument in a contract file makes the file a place
to relitigate rather than a place to look up behavior.

Unknowns behave the same way. An unknown collected into an appendix at the end of a file
is read by nobody who is working on the section it affects, and it survives long past its
resolution because nothing in the section it constrains points at it.

## Decision

`spec/status.md` is generated, and it is the sole statement of performed, committed and
broken. `spec/pins.toml` maps a clause id to the one artifact demonstrating it — a test
function path, a theorem constant, or a type path. A clause with no pin computes
`committed`, so an empty tree computes a clean corpus with no authorial action. A clause
computes `performed` when its pin resolves and every backticked identifier it names that
the registry marks as resolving in code is defined in a definition position. A pin that
does not resolve, and a resolving pin whose clause names a code term the tree no longer
defines, compute `broken` and red the gate — a rename is what reds it.

Every argument lives under `spec/decisions/`, one record per decision. A contract file
states behavior and names no alternative; the rationale tokens raise `SpecRationaleLeak`
outside the records, while `rather than` and `instead of` stay legal so a statement can
say what a behavior is not. Every refusal whose direction is a choice carries a
`decided-by` citation, and a record no clause cites is an orphan. An unknown is one inline
line in the section it affects, and the headings that would collect unknowns elsewhere are
refused.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Pins resolving against the tree, status generated; argument confined to records** *(chosen)* | The conservative verdict is the one produced by doing nothing, and the frequent event — a rename — is the event that reds the gate. Behavior and argument are separately readable and separately revisable. | A clause reaches `performed` only when somebody writes a pin, so coverage is a number the corpus reports about itself rather than assumes. A floor is needed to stop a failing pin being deleted instead of fixed. |
| A status marker an author writes beside each claim | Zero machinery; the marker is right where the claim is. | Lost on maintenance: correct on the day it is written and never again. A rename leaves every marker green. |
| A per-file status header | Cheaper than per-clause; one edit per file. | Lost on grain: one file states both performed and committed behavior, often in adjacent rows, so no single header is true of it. |
| Status derived from whether a named identifier exists, with no pin at all | No pin file to maintain; fully automatic. | Lost on precision: the presence of a type is not evidence that a refusal fires. Identifier existence proves a shape, never a bound. |
| Rationale inline, marked off by convention | The argument sits where the behavior is; no second file to open. | Lost on rot: the argument and the behavior have different lifetimes, and inline rationale makes the contract file the place the decision is relitigated. |

## Criteria

1. **Maintenance by the frequent event** — whether the thing that happens constantly is the
   thing that updates or invalidates the state. **This criterion decided.** Renames happen
   weekly and prose edits are silent; a pin that resolves against the tree converts the
   common event into a red gate, where every hand-maintained alternative converts it into
   nothing at all.
2. **Survivability with no memory** — whether the boundary between performed and committed
   still holds after a year in which no author remembers writing any of it.
3. **Conservative default** — whether doing nothing produces the cautious classification.
   Unpinned computes `committed`, so an unproven claim is never reported as demonstrated.
4. **Grain** — whether the unit of state matches the unit of behavior. Per-clause matches;
   per-file does not.
5. **Argument lifetime** — whether the reasoning can be revised without touching behavior,
   and read without being mistaken for behavior.

## Consequences

Coverage becomes a number with a floor that rises by explicit command after a clean run,
so a verdict cannot be cleared by deleting its pin. A reader of any contract file gets
behavior with no argument in it, and a reader wanting the argument gets one record with
its criteria and its rejected options intact. Unknowns stay where they apply and are
counted per file, so an unresolved question is visible to exactly the person who would
resolve it.

The cost accepted: demonstration is authorial work that nothing forces up-front. Most
clauses sit at `committed` until someone writes a pin, and the corpus therefore has to
publish its own thinness rather than hide it behind a marker that says otherwise. The
second cost is friction on refusals — a refusal whose direction is a choice cannot land
until its record does, so adding one guard is two files.

Reversing the pin mechanism is cheap; reversing the rationale confinement is not, since
every record would have to be folded back into the files its clauses live in, and the
duplication checks would then fire on the folded text.

## Revisit triggers

- The pinned fraction stays flat across a release while the tree grows, indicating pins are
  being avoided rather than written.
- `SpecBrokenPin` fires predominantly on identifier drift with no behavior change, so the
  gate's red is noise rather than signal.
- A record's argument is repeatedly needed inside a contract file to make the behavior
  legible, meaning the pointer is not carrying enough.
