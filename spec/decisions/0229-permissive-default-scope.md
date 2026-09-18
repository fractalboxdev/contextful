# 0229 — A permissive zone default is refused wherever more than one principal owns the store

**Status:** accepted 2026-09-18
**Decides:** `enforcement.place.refusal.permissive-default-with-several-principals`

## Context

A table that declares no inference zone policy resolves to the fail-closed pair — a local
device and an on-premises environment — and reaches no cloud category until an operator
widens it. That resolution applies to tables discovered after startup as well, synthesized
memory among them, so the unlabeled case is the common case rather than an edge one.

A deployment can invert that by setting a project-wide default of `*`, which admits every
constructor including undeclared. On a single-owner store this is coherent: the one
principal who wrote the rows, chose the model and set the default is the only party whose
data can leave, and requiring them to label every table before anything works buys that
party nothing they did not already decide.

Once a second principal holds rows in the same store, the default stops being
self-regarding. The operator who sets `*` is deciding where a colleague's rows are
processed, and the mechanism that carries that decision is a project-level setting rather
than anything attached to the rows themselves. A per-principal allow-set exists and pins a
principal's rows wherever they land, but it is opt-in — a principal who never declares one
is covered by whatever the project default says.

The distinguishing fact is therefore not the width of the setting but who bears its
consequence, and the store already knows how many principals own rows in it.

## Decision

A permissive default resolving an unlabeled table to `*` raises
`EnforcePermissiveZoneDefault` wherever more than one principal owns the store. The same
default stands where a single principal owns everything. Widening an individual table past
the fail-closed pair remains available in both cases; it is the blanket resolution of
everything unlabeled that the multi-principal store refuses.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the permissive default where several principals own the store, permit it where one does** *(chosen)* | The party who bears the consequence of a wide default is the party who set it. A single-owner deployment runs with no labeling work at all. | The refusal depends on a count of owning principals, so a store crosses the boundary the moment a second principal writes a row, and a working configuration stops working at that moment. |
| Refuse a permissive default in every deployment | One rule, no principal count to compute, no configuration that changes meaning as the store grows. | Loses on the cost criterion for a single-owner deployment: labeling every table protects that owner from a model they themselves chose, which is setup work with no consequence behind it. |
| Permit it everywhere and emit a warning | No configuration is refused, and the operator is told. | Loses on consent: the warning reaches the operator who chose the setting, not the principals whose rows it widens, and nothing stops the reads it enables. |
| Permit a wildcard per table rather than per project | Widening stays possible without a project-level switch. | Loses on redundancy rather than on safety — a per-table allow-set carrying `*` is already the ordinary widening path, so a separate default adds a second spelling for one behavior. |

## Criteria

1. **Who bears the consequence** — whether the party setting the default is the party
   whose rows the default can send to a vendor model. *This criterion decides.* A wide
   default over one's own data is a choice; over another principal's data it is a decision
   made about them, and a specification that treats the two cases identically either
   obstructs the first or licenses the second.
2. **Setup cost proportional to benefit** — how much labeling a deployment performs before
   any table is readable, against what that labeling protects.
3. **Redundancy of mechanism** — whether a widening path duplicates one that already
   exists.
4. **Legibility** — whether the resulting placement of any given table can be read off the
   manifest without reasoning about a project-level default.

## Consequences

A single-owner store keeps a one-line escape from the fail-closed resolution and needs no
per-table declarations, which is the case where labeling would be pure overhead.

A multi-principal deployment labels its tables before any of them reach a cloud model, and
that work grows with the table count. It also arrives abruptly: a store that ran fine as a
personal one refuses its default when the second principal appears, and the operator meets
the refusal at that moment rather than at setup.

The accepted cost is exactly that labeling work, plus a rule whose applicability is
computed from store contents rather than written in the manifest. Reversing toward a
blanket permissive default is expensive because the per-principal allow-set and the
protected-class floor both assume the unlabeled case is narrow; loosening the default
changes what those mechanisms are protecting against.

## Revisit triggers

- Per-principal allow-sets become mandatory rather than optional, which would make each
  principal's own declaration — not the project default — the thing that governs their
  rows, and remove the consent argument this record rests on.
- The labeling cost at real table counts is measured and lands high enough that
  multi-principal deployments route around the engine rather than declare.
- A deployment shape appears where several principals own rows but one of them holds
  explicit standing to decide placement for the others, which the principal count cannot
  distinguish from the case this record refuses.
