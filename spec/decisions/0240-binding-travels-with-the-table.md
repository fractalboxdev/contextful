# 0240 — The visibility binding attaches to the table, not to the reading face

**Status:** accepted 2026-09-18
**Decides:** `visibility.mirror.refusal.unbound-table`, `visibility.mirror.refusal.incomplete-binding`

## Context

A table served on an organization-wide face needs three facts before it can be filtered
faithfully: which source governs it, which column names the governed object, and what
fidelity the mapping claims. Those facts travel as `[pipeline.tables.visibility]`, alongside
the source's grain, an optional family, an age budget and the posture past that budget.

The facts are properties of the table. They are known by whoever landed it — the pack author
who projected the source's permission model onto the access tables — and they do not change
when a second face starts reading. A face, by contrast, knows which tables it serves and
nothing about how any of them were governed upstream.

A deployment becomes an organization-wide face at the moment any table declares a visibility
block or any permission sweep exists for any source. That transition is not an installation
step; it happens when a pack lands. So the moment a deployment crosses into needing these
bindings is also a moment when tables already exist, some of them bound and some not.

Two failure shapes follow from that. A table on such a face carrying no binding at all is
served with no mirrored filter, which means its rows reach every reader. And a binding that
claims a servable fidelity while missing what makes the claim checkable — no declared sweep
for its source, so nothing produces the access rows; no `max_acl_staleness`, so the age
budget bounds nothing — is a declaration that looks complete and enforces nothing.

## Decision

The visibility binding belongs to the table and arrives with the pack that lands it, so a
face added later inherits it rather than declaring it. One table carries one binding and one
governing regime. On an organization-wide face, a table carrying no visibility block raises
`VisibilityUnboundTable`, from the diagnose command and from the serving guardrail at start,
naming the table. A binding at a servable level without a declared sweep for its source, or
without `max_acl_staleness`, raises `VisibilityBindingIncomplete`. A table that belongs on
such a face without a servable binding declares `fidelity = "excluded"` and is neither
ingested nor queried.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The binding rides on the table, with an unbound table and an incomplete binding both refused** *(chosen)* | The safe configuration is the one that arrives by default: a table lands filtered, and a new face inherits the filter without its author knowing the source's permission model. | One table cannot be served at per-person fidelity on one face and at cohort fidelity on another, which face-level binding would permit. |
| Face-level bindings | Each face declares what it serves and how faithfully, which matches how a face author thinks about their own surface. | Loses on default safety: a table is unbound until every face declares it, so a new face is exactly the moment the declaration is forgotten, and the failure is silent over-serving. |
| Document the requirement without refusing | No configuration is rejected; operators who read the documentation get it right. | Loses on the same criterion — the failure mode is an unbound table serving rows to everyone, and documentation does not distinguish a deployment that read it from one that did not. |
| Bind at the table, default an unbound table to deny rather than refusing | Fail-closed with no refusal, and the deployment keeps running. | Loses on diagnosability: a table returning nothing is indistinguishable from a table whose reader reaches nothing, so an operator debugs a permission problem that is a missing declaration. |
| Accept a servable binding without a sweep or a budget, enforcing whatever exists | Fewer refusals; partial mirroring beats none. | Loses on checkability of the claim: with no sweep there are no access rows to join against, and with no budget the freshness the binding claims is unbounded, so the stated fidelity is unrelated to what is enforced. |

## Criteria

1. **Whether the safe configuration is the one that arrives by default** — whether a table
   is filtered without anyone acting, or unfiltered until everyone acts. *This criterion
   decides.* The face count grows and the table's governing source does not change, so
   attaching the declaration to the growing side guarantees the gap reappears; attaching it
   to the stable side closes it once.
2. **Whether a declared fidelity claim is checkable** — whether the inputs the claim depends
   on are present.
3. **Diagnosability of a misconfiguration** — whether a missing declaration looks like a
   missing declaration.
4. **Per-face flexibility** — the criterion the chosen option loses on.

## Consequences

A pack that lands a source lands its governance with it, so a deployment that adds a new
face gets filtered tables without the face's author consulting the source's permission
model. The refusals fire at two moments — from the diagnose command, where an operator is
looking, and from the serving guardrail at start, where nobody is — so a misconfigured
deployment fails to serve rather than serving unbound rows.

The cost accepted is uniformity. One table carries one binding and one governing regime,
so a table cannot be served at per-person fidelity to an internal face and at cohort
fidelity to a broader one; a deployment wanting that lands two tables. Face-level binding is
precisely the thing that would permit it, and this record rejects that.

The start-time guardrail also converts a configuration mistake into an availability event.
A pack landing an unbound table takes the next start down, and the operator restores service
by declaring the binding or marking the table excluded.

Reversing toward face-level binding is expensive because every table now carries its
declaration and every face relies on inheriting it; moving the declaration would leave every
existing face silently unbound at the moment of the change.

## Revisit triggers

- Deployments are observed landing duplicate tables solely to serve one source at two
  fidelities, which would make the per-face flexibility cost the dominant one.
- Start-time refusals from newly landed packs become a recurring availability event rather
  than a rare one, which would argue for the diagnose command as the sole enforcement moment.
- A fidelity level appears whose claim is checkable without a declared sweep, which would
  narrow what the incomplete-binding refusal needs to require.
