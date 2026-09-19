# D39 — Aggregate disclosure is enforced at build with noisy thresholding and an up-front budget reservation

**Status:** accepted

## Context

A published aggregate over people discloses an individual through a small group, a dominant contributor, repeated releases over the same units, or a cell whose presence depends on an exact count. A threshold on exact counts before noise is not differentially private: a group's appearance alone reveals a contributor. A budget debited after a release lets concurrent releases overspend it.

## Decision

Disclosure is a property of the staged bytes, enforced once, when `disclosure.release` builds a derived table.

- The build stages no breaching cell: partition selection is a noisy threshold over contributor-bounded counts, the contributor breakdown rolls up after suppression, and the run records a hash over the declared policy.
- A release reserves its per-unit spend for every contributing unit in one catalog transaction before reading; a failed reservation refuses the release with `DisclosureUnitBudgetExhausted`. No unit is skipped silently.
- `max_contributor_share` withholds a concentrated group; unavailable per-contributor masses withhold it as unverifiable.
- A table is governed `per-person` or `cohort`, never both; a cohort read widening an under-floor group raises `DisclosureCohortWidening`, and a singleton cohort key raises `DisclosureSingletonCohort` at declaration.
- The offline diagnostic fails a published aggregate model carrying no policy.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| Build-time enforcement, noisy partition selection, reservation up front *(chosen)* | — | A policy edit is a rebuild; published figures carry noise; an exhausted unit blocks every release naming it. |
| Suppress on exact counts, then add noise | Privacy guarantee | Cell presence would disclose whether a contributor crossed the threshold. |
| Debit spend after the release | Concurrency | Two concurrent releases would each observe budget the other spends. |
| Skip exhausted units silently | Truthfulness | A result would read as complete while its shortfall reveals which units spent their budget. |
| A query-time aggregate evaluator | Survival across read paths | It would be reimplemented per surface and cannot recover per-contributor mass from a rollup. |

## Consequences

- Every read path inherits the guarantee from the staged bytes.
- Raising a unit's lifetime cap is an explicit grant edit.
- A model author writes a shape wider than the published one.

## Revisit

- Lookalike segment release settles its audience floor, readback rule and cross-party consent (the unsettled line under `disclosure.release`).
