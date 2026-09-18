# 0319 — The proof gate is a pinned constant inventory plus a transitive axiom audit, rechecked from pinned source

**Status:** accepted 2026-09-18
**Decides:** `formal.audit-axioms.refusal.axiom-outside-the-allowlist`, `formal.audit-axioms.refusal.source-text-verdict`, `formal.audit-axioms.refusal.hole-axiom`, `formal.audit-axioms.refusal.native-evaluation-axiom`, `formal.audit-axioms.refusal.missing-constant`, `formal.audit-axioms.refusal.statement-drift`, `formal.audit-axioms.refusal.declaration-count-verdict`, `formal.recheck.refusal.credentialed-environment`, `formal.recheck.refusal.artifact-mismatch`

## Context

A proof gate is an instrument, and an instrument is defined by what it fails. The failure
that matters is a package that elaborates cleanly, exits zero, and proves nothing — or
proves `False`. Elaboration reports an unfilled hole as a warning and exits zero, so the
build's exit status carries no information about holes at all. Any gate resting on it is
measuring that the file parses.

Three properties of the elaborated environment are decidable and are the ones that matter.
An axiom footprint is a transitive fact about a proof: every axiom a constant's proof
reaches, whatever the source spells. A constant's presence is a fact about the environment:
deleting a theorem removes the proof. A constant's statement is likewise elaborated text,
comparable against an expected rendering.

Against those, the facts available from the source characters are weak and defeasible. A
hole closed inside parentheses defeats a pattern anchored at a line end. A
native-evaluation tactic spelled with a leading plus defeats a pattern spelling the older
tactic name. Axioms the toolchain has minted since the pattern was written are missed
entirely, because a pattern only finds what its author already knew to look for. The same
weakness applies to counting declarations: a positive count of theorems and lemmas
establishes that a file holds declarations.

There is a second way to pass without proving: weaken the statement until the proof closes.
That is invisible to an axiom audit, because the weakened theorem is honestly proved. It is
only catchable if the required statement is recorded somewhere that editing the declaration
does not also edit.

Finally, a verdict has to be about the commit rather than about the environment that
produced it. A first-phase environment may hold artifacts from an earlier build, and a
gate environment that holds credentials can reach things a rebuild should not.

## Decision

The gate decides on the elaborated environment. An inventory of required constants carries
each one's module, its expected statement as text, and the axioms it is admitted to reach,
maintained apart from any declaration that would satisfy it. The audit reads each required
constant's footprint transitively and matches it against an allowlist of exactly three
entries. A missing constant raises `TheoremConstantMissing`, a drifted statement raises
`TheoremStatementDrift` printing both, an axiom outside the allowlist raises
`AxiomOutsideAllowlist`, and the hole and native-evaluation axioms raise
`ProofHoleAxiom` and `NativeEvaluationAxiom` whatever the surrounding syntax spells. A
verdict derived from a search over source characters raises `SourceTextVerdict`, and a
declaration count offered as a pass condition raises `DeclarationCountVerdict`. The check
then rebuilds from the commit's own source in an environment holding no credential —
raising `RecheckEnvironmentCredentialed` if one is exposed and `ArtifactSourceMismatch` if
a rebuilt artifact diverges from the committed one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Inventory match plus transitive axiom audit over the elaborated environment, then a credential-free rebuild** *(chosen)* | A proof of `False`, a hole, a deleted theorem and a weakened statement each fail loudly, and the verdict is about the commit rather than the machine. | The inventory is maintained by hand and moves whenever a required statement changes, which is friction on exactly the edit that would weaken a claim. Rechecking artifacts and rebuilding from pinned source costs a second elaboration. |
| A source pattern over the package files for holes, admits and native decision tactics | Runs in milliseconds, needs no elaboration, and is readable by anyone. | Lost on failing a proof of `False`: a hole inside parentheses defeats a line-anchored pattern, a tactic spelled with a leading plus defeats a pattern naming the older spelling, and axioms minted since are missed entirely. |
| Counting theorem and lemma declarations, requiring a positive count | Trivially cheap and stable across refactors. | Lost on relevance: it measures that a file holds declarations, which is true of a file whose every proof is a hole. |
| Building alone and reading the exit status | No inventory, no allowlist, no second artifact. | Lost on failing a proof of `False` again: a hole elaborates as a warning and the build exits zero, so the status is uninformative about the one thing being checked. |
| An inventory generated from the declarations themselves | No hand maintenance, and no drift between the two. | Lost on independence: a generated inventory follows a weakened statement, so the one failure mode an axiom audit cannot see stays invisible. |

## Criteria

1. **Whether the gate fails a proof of `False`** — whether the instrument catches the case
   it exists for. *This criterion decided it.* Every rejected option is cheaper and every
   one of them passes a package whose proofs are holes; a gate that cannot fail its own
   defining case provides assurance without providing evidence, which is worse than an
   absent gate because it is quoted.
2. **Independence of the inventory from the declarations satisfying it** — whether
   weakening a definition can make a proof pass. Second, and decisive between the two
   inventory shapes.
3. **Elaborated fact against source fact** — whether the input is what was proved or what
   the file spells.
4. **Reproducibility of the verdict** — whether a rebuild from pinned source in a
   credential-free environment reaches the same answer.
5. **Cost of the check** — elaboration time and artifacts. Real, and outranked by the
   first criterion; it constrains the package rather than the instrument.

## Consequences

The gate reports per constant rather than in aggregate, so an operator reads a row naming
the constant, whether its statement matched, and every axiom it reached. A report states
the allowlist it applied and the inventory revision it matched against, so a verdict read
later resolves without the environment that produced it. `Classical.choice` sits in the
allowlist as an explicit reviewed addition recorded with the constant that draws on it,
rather than as a default nobody examined.

The accepted cost is hand maintenance and a second elaboration. Changing a required
statement is two edits in one commit, with the report printing the old text beside the new
one — deliberate friction, placed on the edit most able to hide a weakened claim. The
recheck doubles the elaboration cost of the gate, which is affordable only because the
package declares no dependency of its own.

## Revisit triggers

- The inventory's hand maintenance is observed to be routinely stale, so required
  statements lag the proofs and the second edit stops being a real check.
- The toolchain adds an axiom that the allowlist's three entries do not anticipate,
  requiring a reviewed addition and a record beside the constant that draws on it.
- The second elaboration pushes the gate outside its per-change budget, which forces a
  choice between the recheck and the cadence.
