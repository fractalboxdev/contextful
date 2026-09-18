# 0222 — A mask naming a column the schema omits is refused at manifest load

**Status:** accepted 2026-09-18
**Decides:** `enforcement.mask.refusal.mask-on-an-absent-column`

## Context

A column mask is declared in the manifest against a table and a column name. The relation
compiler reads that declaration and substitutes the masked expression for the named column
where it stands in the projection, so the column set a caller receives, and its order, match
what the same statement produces unmasked. A star projection expands to the masked projection.
The binding between declaration and column is by name, and nothing else.

Names move. A table's schema is produced by a pipeline, and a rename upstream — a source field
relabeled, a normalization step changing how a structured value shreds into child tables —
changes the column set without touching the manifest. The mask declaration survives the rename
intact and now names nothing.

What happens next decides whether the operator learns anything. A declaration that names
nothing and is quietly skipped leaves the manifest reading exactly as it did before: the
operator opens it, sees a mask on a sensitive column, and believes the column is protected.
The column is in the result in cleartext. This is not an exotic failure — a rename is the
ordinary way it happens, and the two artifacts that must stay in step live in different places
and change for different reasons.

The other direction of failure is a refusal that fires during ordinary schema work, which is
noisy but visible, and visible at the moment and in the file where the mismatch was created.

## Decision

A declared mask naming a column the table's schema omits raises `EnforceMaskOnAbsentColumn` at
manifest load. The manifest is refused as a whole rather than loaded with the dangling
declaration skipped, so a deployment never serves reads under a policy whose author believed it
covered a column it does not name.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at manifest load, naming the column and the table** *(chosen)* | The mismatch surfaces in the file where it was created, while the operator who owns both artifacts is present. Failure is closed. | A rename of a masked column reds the manifest until the policy is updated alongside it. |
| Ignore the declaration | Schema changes never break a manifest; renames land without policy work. | Loses on failure direction: the operator reads a mask that is not applied and believes a column is protected. A rename is exactly how it disappears. |
| Create the column | The declaration always has a target, so the mask is always applied to something. | Loses on ownership of the schema: a policy file is not a schema, and a fabricated column carries no values while presenting as a real one. |
| Refuse at first read | No load-time pass; the failure lands where the column would have been served. | Loses on who is present: the operator who wrote the mask is no longer there, the caller cannot repair a declaration, and a rarely-read table hides the mismatch indefinitely. |

## Criteria

1. **Failure direction after a schema change** — whether the mismatch fails open, leaving a
   column unprotected, or closed. This is the criterion that decided it.
2. **Who is present when the failure surfaces** — the operator holding both artifacts, or a
   caller holding neither. Refusal at first read fails this.
3. **Which artifact owns the schema** — whether a policy file can bring a column into
   existence. Creating the column fails this.
4. **Tolerance of ordinary schema work** — how often a legitimate change is interrupted. This
   is the criterion the chosen option loses on.

Failure direction decides it. The other options each produce a running system; only one of them
produces a running system whose operator holds a false belief about which columns are
protected. A mask exists precisely to be relied upon, and a silently-inapplicable mask is worse
than no mask, because no mask prompts the question.

## Consequences

A manifest that loads is a manifest whose every mask has a target, so the set of protected
columns is exactly what the file says it is. The refusal names the table and the column, which
makes the repair mechanical: rename in the policy, or restore the column.

The accepted cost: a rename of a masked column reds the manifest until the policy is updated
alongside it. Schema work and policy work are therefore coupled, and a pipeline change that
would otherwise land alone now lands with a manifest edit beside it. In a deployment where
those two are owned by different people, that coupling is a handoff rather than an edit.

Reversing this toward silent skipping is cheap to implement and cannot be done safely after the
fact: every manifest that loaded under the refusal would keep loading, and the first rename
after the change would remove a protection with no signal.

## Revisit triggers

- The manifest gains a way to bind a mask to something more durable than a column name, so a
  rename carries the declaration with it.
- Schema and policy ownership are observed to sit with different parties often enough that the
  coupling delays protective changes rather than enforcing them.
- A legitimate case appears for declaring a mask ahead of the column existing — a table that
  acquires columns over time — which the load-time refusal forbids.
