# 0042 — The request-ledger child relation registers on the owner read alone

**Status:** accepted 2026-09-18
**Decides:** `read.register.refusal.scoped-ledger`

## Context

Every table carries a per-run request ledger recording the mediated outbound calls a run
made: identifiers, connector, method, host, status and timing. It registers as the child
relation `<table>__requests` so an operator can read it with ordinary SQL rather than a
bespoke surface.

The ledger's columns describe vendor traffic, not subject data. There is no tenant column in
it, and there is no column that could stand in for one — a connector's host and method are
properties of the run, and a run pulls for whatever tenants its pipeline covers. Adding a
tenant dimension is not a matter of naming an existing column; the information is not in the
rows.

That matters because every other relation on this face is narrowed the same way: a grant
allowlist decides which tables register, and a row predicate composed into the relation
decides which rows a tenant-scoped caller sees. A relation with no column the predicate can
bind to cannot be narrowed at all. Registering the ledger as an ordinary relation would
therefore hand a tenant-scoped caller the full ledger, which is a reasonably detailed map of
every tenant's vendor traffic shape: which vendors are called, how often, with what latency
and what error rate.

There is also a second read surface problem. Returning an empty ledger to a scoped caller
would narrow nothing while appearing to answer, and an empty ledger reads as a factual claim —
that no vendor calls happened — which is exactly what it is not.

## Decision

The child relation registers on the owner read alone: no grant allowlist, no row predicate,
no column mask, no tenant scope. A tenant-scoped token naming it raises
`LedgerNotTenantScoped` and states what closed the relation, rather than returning an empty
result.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Owner read only; a scoped token is refused with its reason** *(chosen)* | No caller reads another tenant's vendor traffic, and a scoped caller learns why rather than inferring from absence. | No existing grant pattern reaches the child relation, so a legitimate scoped consumer has no route to its own calls. |
| Register the ledger as an ordinary relation | Uniform treatment: one registration rule for every relation this face offers. | Loses on cross-tenant isolation — a tenant-scoped grant has no column to filter by, so one tenant reads every tenant's vendor traffic. |
| Return an empty ledger to a scoped caller | The relation exists for every caller and no surface refuses. | Loses on honesty: absence reads as no vendor calls having happened, and a consumer cannot distinguish the two. |

## Criteria

1. **Cross-tenant isolation** — whether a second read surface answers what the first
   withholds. Ordinary registration fails this.
2. **Honesty of an empty result** — whether absence of rows is a fact about the store or an
   artifact of the caller's scope. Elsewhere on this face returning fewer rows than requested,
   zero included, is a success a caller may act on, which is precisely why an empty ledger
   cannot be used as a restriction mechanism.
3. **Reach for a legitimate scoped consumer** — whether a tenant can audit its own outbound
   calls. This is the criterion the chosen option loses on.

Cross-tenant isolation decides it, and the ledger carrying no tenant column decides it
outright: there is no version of scoped registration that is merely harder to implement. The
column is not there.

## Consequences

The ledger is exactly one thing — an operator's view of the store's outbound behavior — and
reasoning about who can read it is a single check rather than a composition of grants. The
refusal names its own reason, so a scoped caller that expected access learns the shape of the
gap rather than filing a bug about an empty table.

The accepted cost is that a tenant cannot audit its own vendor calls through this face at all,
and widening it is not a grant change. The tenant dimension has to reach ledger rows first,
which means the connector recording each call has to know and record which tenant the call was
made for — information the run-level ledger does not currently carry and that some connectors
may not have.

## Revisit triggers

- Ledger rows acquire a tenant column populated at the point the outbound call is recorded,
  making a row predicate expressible.
- A tenant-facing audit obligation requires per-tenant visibility into outbound calls, which
  would force the column question rather than leave it open.
- The ledger's columns are narrowed to a set carrying no cross-tenant signal, which would make
  a wider registration defensible on its own terms.
