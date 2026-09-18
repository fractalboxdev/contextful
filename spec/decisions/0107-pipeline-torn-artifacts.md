# 0107 — Disagreeing published-table artifacts omit the table from the manifest section

**Status:** accepted 2026-09-18
**Decides:** `pipeline.publish.refusal.manifest-section`

## Context

A published table carries five artifacts beside its data: `contract.json`,
`contract-history.jsonl`, `freshness.json`, `builds.jsonl` and `holds.jsonl`. They are
separate files written at different moments in a build, and nothing makes the set of
writes atomic. A crash between them, a partial sync, a restore from an inconsistent
backup — each leaves a table whose artifacts describe different builds.

The manifest section is where a consumer reads that state: `{contract_version,
schema_fingerprint, build_id, last_built_at, watermark, max_lag, last_build_status,
partitions_failed?, semantics_version?, fingerprint_recipe?}`. It is assembled from those
artifacts, and its identifiers name the newest publishing entry in the build log and never
a refused one.

Disagreement has a specific shape. A freshness record names a build the log does not
carry. A contract identity appears that no entry published under. In both cases the
artifacts are individually well-formed and collectively impossible, and the engine has no
basis for deciding which one is right.

A consumer reading the manifest is making a decision with it — whether to query, whether
the data is fresh enough, which identity to bind. The manifest's optional keys already
establish the rule the section follows: a key is absent rather than null where the
artifact predates it, and neither optional key is inferred from the other. Absence is
already the section's vocabulary for "this is not known".

## Decision

Artifacts that disagree — a freshness record naming a build the log does not carry, or a
contract identity no entry published under — raise `PipelineModelArtifactsTorn`. The
table drops out of the manifest section entirely rather than appearing with a
reconstructed value.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Omit the table from the section** *(chosen)* | A table present in the section is fully described by artifacts that agree; absence is unambiguous and already the section's vocabulary | A single damaged artifact hides an otherwise healthy table from every consumer reading the manifest, and repair means rebuilding rather than editing |
| Emit the section with the fields that agree | The table stays visible; partial information is better than none | Lost on distinguishability: a section carrying a subset of keys is indistinguishable from a complete description of a table nobody verified, and consumers read it as one |
| Infer the missing identifier from the newest artifact present | The section is complete; the inference is usually right | Lost on exactly the failing case: a torn write is the one situation in which the newest artifact is not the authoritative one, so the inference is wrong precisely when it is invoked |
| Emit the section with an explicit torn marker | Visible and honest; consumers can branch on it | Lost on reach: it adds a state every consumer must handle to stay correct, and a consumer that ignores the marker reads a reconstructed description as a real one — the same failure as emitting partial fields |

## Criteria

1. **Whether a consumer can distinguish an absent table from one described by a
   reconstructed value.** *(decided it)*
2. **Correctness of any inference in the failing case.**
3. **Availability of a healthy table** — what the choice hides.
4. **Burden placed on every consumer.**

Distinguishability decided it because absence is unambiguous and an inferred value is not.
A consumer that finds no section for a table knows it has nothing and behaves
accordingly; a consumer handed a partly-reconstructed section cannot tell it from a
verified one and proceeds on it. Correctness of the inference then rules out the
reconstruction option on its own terms: the argument for inferring is that the newest
artifact is authoritative, and a torn write is defined by that assumption failing.

The torn marker lost on burden rather than on honesty — it is honest, and it is a state
every consumer would have to learn, with a default behavior that fails the same way the
partial section does.

## Consequences

A table present in the manifest section is described by artifacts that agree with each
other, so a consumer needs no verification step and no torn-state branch.

The refusal names the disagreement, so an operator reading the error learns which
artifact contradicts which rather than that something is wrong with the table.

The cost accepted is availability. A single damaged artifact hides an otherwise healthy
table from every consumer reading the manifest, and the data beneath it may be complete
and current. The table is invisible for a reason unrelated to its contents.

Repair is also not an edit. The artifacts are the record of what builds happened; hand-
writing a build-log entry to satisfy the check would manufacture the agreement rather
than restore it. The recovery is a rebuild, which for a large table is real time and
real spend.

Reversing toward emitting partial sections is cheap in code and expensive afterwards:
every consumer that grew a tolerance for missing keys would then read a genuinely torn
table as merely an old one.

## Revisit triggers

- Artifact writes become atomic as a set, which removes the torn state rather than
  handling it and makes this refusal unreachable.
- Torn artifacts are observed often enough that hidden healthy tables become a real
  availability cost rather than a rare one.
- A repair verb appears that can reconstruct a consistent artifact set with its own audit
  trail, which turns recovery from a rebuild into an operation.
