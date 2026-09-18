# 0105 — A committed seed whose source fingerprint moved refuses the run

**Status:** accepted 2026-09-18
**Decides:** `pipeline.seed.refusal.fingerprint`

## Context

A seed commits once. After it commits, every subsequent run of the pipeline finds a
seeded table already loaded and has to decide whether to look at the export again.

The export does not stay still. An operator who discovers the first export was short by a
year appends the missing history and re-runs. A vendor regenerates a dump on a schedule.
A file is replaced in place with a corrected version. In each case the file at the
declared path now holds rows the store does not.

The naive behavior — skip the load because the seed is committed — makes that invisible.
The operator appends history, re-runs, sees a clean exit and a successful run, and
believes the top-up loaded. Nothing in the run record contradicts them. That specific
sequence is the failure class this check exists for.

Re-landing is not dangerous. The seeded table carries a mandatory key, so the fold
collapses a re-load of rows already present, and the ceiling still governs what may land.
What re-landing costs is time and money: a large export is hours of work and, for an
object-store source, egress and request charges. That is an operator's call rather than
the engine's.

The detector is also weaker than the guarantee it serves. Each run probes the source for
a cheap fingerprint — a filesystem stat for a local export, a HEAD for an object-store
one, never a download — and compares it with the fingerprint recorded at commit. Stamping
on commit alone is what keeps a half-loaded export from claiming its whole contents
landed.

## Decision

A fingerprint differing from the recorded one raises `PipelineSeedSourceChanged`, naming
both values and pointing at the reset verb. An equal fingerprint skips the load. A
fingerprint the source cannot supply skips and warns each run that a top-up here would go
unnoticed.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse on change, point at the reset verb** *(chosen)* | The append-and-re-run sequence is caught; the decision to spend hours re-loading stays with the operator | An operator whose export legitimately churns hits a refusal every run until they reset or repoint |
| Skip unconditionally once committed | Cheapest; no probe, no refusal, no state to keep | Lost on detection: an appended top-up is never noticed, which is exactly the failure class the probe exists for |
| Re-load automatically on change | The store always matches the export; no operator action | Lost on cost control: it spends hours and vendor quota unasked on every touch of the export, including a re-generated dump whose contents are identical |
| Warn on change and skip | Nothing blocks; the fact is recorded | Lost on where the signal lands: a warning on a cadence-driven run is read by nobody, and the operator's mental model — "I appended and re-ran" — goes uncorrected |
| Fingerprint by content digest | A real guarantee rather than a detector | Lost on cost: it downloads the whole export every run, which is the expense the skip path exists to avoid |

## Criteria

1. **Whether the append-and-re-run sequence is caught** — the specific failure the check
   exists for. *(decided it)*
2. **Who decides to spend hours and vendor quota.**
3. **Probe cost per run.**
4. **Strength of the detector against a determined change.**

Catching the sequence decided it because it is the only criterion on which the cheap
option fails outright rather than scoring lower. An operator who appends history and sees
a clean exit has been actively misled, and every later question asked of the table is
answered over a history they believe is there. Ownership of the spend then settled the
shape between refusing and re-loading: re-landing is safe but expensive, and an expensive
safe action taken without asking is still taken without asking.

## Consequences

The append-and-re-run sequence ends in a refusal naming both fingerprints and the verb
that clears the seed's chunk plan, so the operator's next action is one command and the
re-load is deliberate.

`seed status` prints each seeded table's ceiling, its commit state and whether its source
still matches, so the state is readable without firing.

The cost accepted is the detector's weakness, and it is worth stating at its real size: a
stat or a HEAD is a change detector rather than a content guarantee. An edit that moves
neither the size nor the reported modification stamp — an in-place correction of equal
length, a re-upload preserving metadata — slips through and the store keeps serving the
old history with a clean fingerprint. A source that can answer only by reading everything
supplies no fingerprint at all and stays on the warn path permanently, where a top-up
goes unnoticed exactly as it would have under the unconditional skip.

An export that churns for reasons unrelated to its contents — a nightly regeneration of
identical rows — refuses every run until the operator resets or repoints, and the engine
offers no way to acknowledge a change as uninteresting.

## Revisit triggers

- A source class appears that supplies a strong content identity cheaply — an entity tag
  that is a digest, a vendor-published export hash — which turns the detector into a
  guarantee for that class.
- Operators are observed resetting routinely against an export that regenerates
  identical contents, which is the churn case the refusal has no answer for.
- The warn path becomes common rather than exceptional, meaning most seeded sources
  cannot be probed and the check's coverage is thinner than this record assumes.
