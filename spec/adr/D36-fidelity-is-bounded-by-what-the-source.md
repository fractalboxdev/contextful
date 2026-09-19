# D36 — Fidelity is bounded by what the source enforces

**Status:** accepted

## Context

Sources expose permission at different grains: exact per-item lists, container rosters, per-person containers, computed sharing engines, or nothing. A single deployment-wide permission-aware claim would be set silently by the weakest source.

## Decision

A table claims only the precision its source enforces at sign-in, and the mirror holds only what the source itself would enforce.

- `disclosure.declare-fidelity` sets one level per table — `mirrored`, `coarse`, `federated`, `excluded` — and a `coarse` envelope names the source's grain.
- The declared `family` bounds the permitted levels at load; an out-of-family level raises `VisibilityFamilyBound`, and an undeclared family refuses.
- A source that computes access is queried live under the reader's delegated credential; mirroring its inputs raises `VisibilityComputedInputsMirrored`. A `federated` table is unregistered on read faces and never cached across subjects.
- A people list inside a content payload confers no read.
- `disclosure.pack` lands every table with an access mapping or `excluded`; an access-shaped key the mapping schema lacks raises `VisibilityPackAssertsAccess`.
- A source with no permission data lands as an ordinary table with no fidelity claim, admitted by credential grant alone.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Per-table level, bounded by a declared family, enforced at load *(chosen)* | — | Every answer carries qualification; federated legs pay live latency and go dark with the source; broad faces lose sources with no permission endpoint. |
| One deployment-level permission-aware claim | Falsifiability | The weakest source would set the real guarantee with nothing detecting it. |
| Reviewer judgment on each level declaration | Failure visibility | A mistake would surface as a disclosure months later. |
| Mirror a sharing engine's inputs and reproduce its rules | Drift | A second evaluator would disagree in exactly the restricting constructs. |
| An audience field in the pack for unmirrorable sources | Enforceability | No read-path step would consult it: a control that controls nothing. |

## Consequences

- One judgment — the family — is made per source; everything downstream is mechanical.
- Authorization has one source: the join over observed rows.
- A reader learns how exact each part of an answer is.
