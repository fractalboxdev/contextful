# 0320 — The assurance claim names decisions, specifications, trusted dependencies and its translation chain

**Status:** accepted 2026-09-18
**Decides:** `formal.scope-claim.refusal.claim-beyond-named-decisions`, `formal.scope-claim.refusal.unnamed-trusted-dependency`, `formal.scope-claim.refusal.unstated-chain`

## Context

The theorems exist to be relied on by someone outside the team: an auditor, a security
reviewer, a buyer's engineer. That reliance happens through one sentence, quoted far from
the tree, long after the proof ran. The sentence is therefore the artifact, and its wording
is a decision rather than a presentation detail.

What the theorems actually reach is bounded and known. The package holds no definition of
credential bytes, signature verification, SQL semantics, journal writes, process state
transitions or an attacker's observations, and no theorem in it reaches any of those. A
sentence asserting that the enforcement engine is verified therefore names an object with
no counterpart in the package at all — a reader asking which constant establishes it finds
nothing, and cannot tell whether the gap is in the proofs or in their own understanding.

Every theorem here also holds given the correctness of things underneath it. The delegation
library, its cryptography and its parser sit beneath the authority mapping. A statement
about translated code inherits a four-link chain: the compiler's lowering, the translator,
the hand-written models standing in for external definitions, and the production build
configuration. Those are not caveats to be recalled if asked; they are conditions of the
statement, and a quotation that drops them is a quotation of a different statement.

The property that makes all of this tractable is resolvability. A named decision resolves
to a code path, a named specification resolves to a constant in the elaborated environment,
a named assumption resolves to a component. A reader checks the claim by opening things,
without asking its author anything.

## Decision

The claim reads as three resolving lists: these named authorization decisions satisfy these
named specifications, under these stated translation and runtime assumptions. A claim
asserting that the enforcement engine is verified, or that any object wider than the named
decisions is, raises `ClaimBeyondNamedDecisions`. A claim resting on a component absent
from its dependency list raises `TrustedDependencyUnnamed`, naming the component, and a
theorem claimed over translated code without its four-link chain raises
`TranslationChainUnstated`. Wherever a theorem appears, its assumption list appears with
it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Three resolving lists: decisions, specifications, assumptions** *(chosen)* | Every noun opens: a decision to a code path, a specification to a constant, an assumption to a component. The claim survives being quoted because its limits are inside it. | The claim is narrower than a buyer would prefer to hear, and every quotation of a theorem carries its assumption list with it. |
| Claiming the enforcement engine is verified | The sentence a buyer wants, with no qualification to explain in a meeting. | Lost on checkability: the models hold no definition of credential bytes, signatures, SQL semantics, journal writes, state transitions or attacker observations, so the claim resolves to nothing a reader can open, and its falsity is discoverable by anyone who looks. |
| Leaving the trusted dependencies implicit | A shorter sentence, with the assumptions available on request. | Lost on checkability for the same reason: a reader cannot tell what the theorem assumes, so they cannot tell which defects it would have caught. Requests do not travel with quotations. |
| Stating the assumptions once, in a document beside the claim | The claim stays short and the conditions are still written down somewhere. | Lost on travel: the quoted unit is the sentence, and conditions kept outside it are the ones a third-hand reader never sees. |

## Criteria

1. **What a reader can check without asking the author** — whether each noun resolves in
   the tree. *This criterion decided it.* The claim's entire purpose is to be relied on at
   a distance, and a term that resolves to nothing converts reliance into trust in the
   author, which is the thing formal methods are purchased to replace. Persuasiveness is
   the genuine advantage of the wider claims and is worth nothing once checked.
2. **Survival under quotation** — whether the sentence keeps its limits when it is lifted.
3. **Completeness of the assumption set** — whether a component the statement rests on can
   be absent from the list.
4. **Persuasiveness to a non-technical buyer** — how strong the sentence sounds. Outranked,
   and the accepted cost.

## Consequences

An auditor reads a sentence whose every term they can verify against the tree, and a
theorem quoted into their packet arrives with the conditions it holds under. The trusted
remainder — the delegation library, its cryptography, its parser, and the four translation
links — is written down as a perimeter rather than discovered during review.

The accepted cost is commercial and conversational. The claim reads narrower than the work
behind it sounds, a reader who wanted "the engine is verified" gets a list of decisions
instead, and anyone quoting a theorem carries an assumption list that makes the quotation
longer. The wide version of the sentence is not available even informally, since the
refusal binds the claim rather than the document it appears in.

## Revisit triggers

- A mediation theorem over an execution relation is proved, which genuinely widens the
  object and lets the claim name a larger one without losing resolvability.
- A named trusted dependency is brought inside the proofs, which shortens the assumption
  list and changes what every quotation carries.
- A reader is observed to misread the scoped claim as wider than it is, which would argue
  for the claim stating its exclusions as explicitly as a theorem states its negative
  space.
