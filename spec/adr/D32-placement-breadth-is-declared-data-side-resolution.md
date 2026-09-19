# D32 — Placement breadth is declared data-side, and resolution fails narrow

**Status:** accepted

## Context

Inference placement decides which model location may read which rows. The caller controls its credential and its requests; the data owner controls table, column and principal declarations. A synthesized row is written by the same path that read its restricted input.

## Decision

Only the data side widens placement, every widening is visible in the manifest diff, and every ambiguity resolves to the narrowest set.

- `authority.issue` refuses a credential or subject declaring a wildcard inference zone; a caller whose zone varies asserts it per request.
- `authority.place` refuses a permissive default for unlabeled tables where more than one principal owns the store.
- Widening a protected-class surface past its floor requires a class-named override flag beside the surface; without it the surface narrows to the floor at serve time.
- A synthesized row's zone set is at most the intersection of its evidence tables' sets; a wider declaration refuses and resolves to the intersection.
- Incognito pins the session, the uncredentialed local owner included; a wider asserted zone refuses.
- No model-vendor client library links into any workspace package; the engine reaches a model through an operator-configured endpoint.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Data-side breadth, intersection for synthesis, refusal of every widening assertion *(chosen)* | — | A multi-zone batch mints per zone; a second principal writing a row breaks a permissive default; one narrow evidence table narrows a whole conclusion. |
| Accept a subject-side wildcard as every zone | Which side widens | A credential would grant itself every zone around the allow-set. |
| Trust a synthesized row's declared set, or the union | Laundering | The constrained path would enforce its own constraint. |
| Honor a wider zone under incognito and audit it | Toggle guarantee | The audit would report a crossing the toggle exists to stop. |
| A vendor client behind a feature flag | Verifiability of the claim | The placement statement would depend on a build the reader cannot see. |

## Consequences

- The override flag records an assertion; no vendor agreement is verified.
- A session needing a cloud model mid-task restarts without the pin.

## Revisit

- Fragment-level provenance through synthesis becomes observable.
- Multi-zone subjects are common enough that per-zone minting dominates issuance.
