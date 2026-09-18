# 0339 — An action outside the vocabulary refuses at the mint and at admission

**Status:** accepted 2026-09-18
**Decides:** `authority.grant.refusal.unknown-action`

## Context

The action vocabulary holds four verbs. `read` covers every row-returning surface, `write`
lands rows, `execute` fires a run, and `admin` mints. The set is closed because each verb names
enforcement sites the engine holds: a verb is not a label on a grant, it is the thing a
call site compares itself against before acting.

An action token outside that set reaches the engine from two directions. A mint request carries
one because a caller asked for a verb by a spelling the engine does not hold — a synonym, a
verb from another system's vocabulary, a typo. A presented credential carries one because it
was minted elsewhere: by a deployment that extended its own set, against a newer profile
version, or by a party assembling a credential to see what the checkpoint does with it.

Skipping the token is safe in the narrow sense that an action nobody enforces authorizes
nothing, so an ignored verb narrows rather than widens. That is what makes it tempting, and it
is what makes the resulting failure unreadable. A credential minted with one action, whose
spelling the engine skips, verifies, admits, carries an empty action set and reads nothing —
a result indistinguishable from a grant over an empty table set, a withdrawn credential, or a
store with no rows. The holder has bytes that pass every check and do nothing, and the defect
presents at the surface furthest from its cause.

Where the check runs is the second question. A mint-side check catches the caller's typo at the
moment a person is present to fix it, and reaches nothing minted by another deployment.

## Decision

An action token outside the vocabulary raises `GrantActionUnknown`, at the mint and at
admission alike. A credential's action set is therefore always a subset of the four verbs the
engine enforces, whichever party minted it and whichever profile version produced it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse at both ends** *(chosen)* | Every verb in an admitted credential names enforcement sites, and a caller learns about a bad spelling at the moment it asks. | Extending the vocabulary is a profile version rather than a deployment-local addition, and a credential minted against a wider set is turned back by a checkpoint that does not hold it. |
| Ignore the unrecognized token and admit the rest | Forward compatibility comes free, and no credential is refused for naming a verb the checkpoint has yet to learn. | Loses on legibility: the credential admits and authorizes nothing, which is the same observable as an unprovisioned grant, so the holder debugs the wrong end of the system. |
| Refuse at the mint alone | The typo is caught where the author is, and admission stays a signature-and-claims check. | Loses on reach: a credential minted by another deployment or under a different profile version arrives at admission carrying whatever its issuer allowed, and the one check that runs over it does not look. |
| Map an unrecognized token onto the nearest known verb | A synonym works, and a caller writing `select` gets the read it meant. | Loses on the direction of the error: the mapping is a guess about semantics, and every guess that resolves toward a broader verb hands the holder authority nobody granted. |
| Let a deployment register additional verbs | Local vocabulary for local surfaces, without a profile bump. | Loses on enforcement: a verb with no call site comparing against it appears in introspection as authority and is honored by nothing, so a grant reads as broader than it is. |

## Criteria

1. **Whether every verb in a credential names an enforcement site** — whether authority as
   declared corresponds to authority as enforced. *This criterion decides.*
2. **Legibility of the failure** — whether a defect presents as an error or as an empty
   result.
3. **Reach** — whether the check covers credentials this deployment did not mint.
4. **Forward compatibility** — whether a checkpoint keeps serving credentials minted against a
   wider vocabulary.

Criterion 1 decides, over criterion 4. Introspection reports the scope a credential declares
without running anything, and the formal model quantifies over the action set; both statements
hold only where the declared set and the enforced set are the same set. Criterion 2 eliminates
the skip on its own — a credential that verifies and then does nothing is the least
diagnosable failure the admission path can produce — and criterion 3 is what puts the check at
admission as well as at the mint, since the mint is not the only place a credential comes from.

## Consequences

An admitted credential's actions are four values, each with call sites behind it, which is what
lets enforcement compare by value rather than by string and lets an audit record name what was
exercised. A caller asking for a verb the engine does not hold is told so at the mint, with the
vocabulary in hand, rather than receiving bytes that quietly do nothing.

The cost accepted is vocabulary rigidity. Adding a verb is a profile version, and until a
checkpoint holds that version it turns back credentials minted against it — so a fleet mints
against its oldest deployed profile. A deployment with a surface that wants its own verb
expresses it by narrowing an existing one, through table patterns and template allowlists,
rather than by naming a new action.

## Revisit triggers

- A surface appears whose authority genuinely does not decompose into the four verbs, so
  narrowing an existing one misstates what is granted.
- Mixed-version fleets make minting against the oldest deployed profile a constraint on
  delivery rather than bookkeeping.
- An introspection path emerges that can report a verb as declared-but-unenforced without the
  holder mistaking it for authority, which would make an ignore-and-report arm satisfiable
  against criterion 1.
