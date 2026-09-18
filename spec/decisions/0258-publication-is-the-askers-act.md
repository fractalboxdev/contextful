# 0258 — An answer reaches an audience by the asker's deliberate act, under their own provenance

**Status:** accepted 2026-09-18
**Decides:** `visibility.publish-answer.refusal.share-affordance-on-a-diagnostic`, `visibility.publish-answer.refusal.askerless-audience`

## Context

An answer is computed against one reader's reachable set. The moment it lands where other
people read it, every guarantee behind it stops applying: no sweep reaches a posted
message, the hosting platform's retention outlives every revocation the mirror records,
and the recipients were never resolved to principals at all.

So the guarantee has an end, and the only question is what stands at it. Nothing the engine
does after publication can narrow a message already sent, which rules out treating
publication as another enforcement point and leaves a choice between defaults.

Two surfaces make the choice concrete. A question typed into a busy room invites a reply
everyone sees, and that reply would carry material drawn from one person's access. An
access explanation is worse: it states that a named person does not reach a named resource,
so a share control on it offers one click from a private diagnostic to a public disclosure
about someone else.

Scheduled posts have no asker at all. A digest fires on a cadence, originates from no
reader, and therefore from no reachable set — so a service identity posting to an audience
is computing an answer at a scope that does not exist and delivering it to people nobody
resolved.

The one mechanism that already handles this correctly is social, not technical. People
understand that repeating something they were told is their own act, and they apply
judgment to it. Matching that norm puts the decision where the judgment is.

## Decision

On a surface carrying an audience the default reply reaches the asker alone, and an answer
of any substance lands in a direct message or a private thread rather than an ephemeral
one. Posting where others read it is a separate act the asker takes after reading the
answer, and the posted answer carries a line stating it was drawn from the sharer's own
access. A surface offering a share control on access-explanation output raises
`VisibilityShareAffordance`. A scheduled job posting to an audience under a service
identity raises `VisibilityAskerlessAudience`, naming the destination. A scheduled post's
corpus is narrowed by manifest policy, read as a diff.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Single-reader default; publication is the asker's explicit act under their provenance** *(chosen)* | The decision sits with the person who holds the context and already applies judgment to repeating things, and the guarantee ends at a point someone chose. | Readers lose the convenience of an answer everyone sees, ephemeral replies do not survive a reload, and scheduled digests carry a manifest-narrowed corpus rather than a reader's scope. |
| Compute the answer at the intersection of every room member's reachable set | An answer safe for everyone present, posted by default. | Loses on three counts: closure cost multiplies by membership and the epoch cache does not help; it needs a synthetic cohort principal that the grant tables and the diagnostic would both have to model; and a member who joins later reads a message computed without them. |
| Infer publicness from how the question was phrased or where it was typed | No extra step for the asker; the common case is frictionless. | Loses on safety: an interaction default doubles as a disclosure policy, so a phrasing habit decides who reads material drawn from one person's access. |
| A per-room configuration flag setting the default | Operators tune it per room, matching local norms. | Loses on where the guarantee ends: the decision moves to configuration written once, far from the message, and applies to answers nobody has read yet. |
| Allow scheduled posts under a service identity, filtered inside the posting application | Digests keep working with no manifest work. | Loses on reviewability: the filter is code in an application rather than policy read as a diff, and it narrows against a scope that does not exist. |

## Criteria

1. **Where the guarantee can actually end.** *This criterion decides.* Revocation cannot
   reach a posted message under any option, so the question is not how to extend
   enforcement but who decides at the boundary — and only a person holding the context can
   weigh the specific answer against the specific room.
2. **Whether a default doubles as a disclosure policy.** An interaction convenience that
   silently decides audience is the failure mode being avoided.
3. **Cost and modelling burden of a cohort scope.** Multiplies by membership, and needs a
   principal shape nothing else in the contract has.
4. **Convenience of the common case.** Given up deliberately.

## Consequences

Substantial answers arrive privately and are forwarded by hand, so a room loses the shared
artifact it would otherwise accumulate and the asker carries the repetition. The provenance
line rides every posted answer and states plainly that it may contain material other people
present do not reach, which is a hedge readers will learn to skim.

The accepted cost is friction on the commonest good case: a colleague asks a harmless
question in a shared room and the useful answer does not appear there. Some of those
answers will simply never be shared, and the deployment is less useful for it.

Scheduled digests pay differently. Their corpus is narrowed by manifest policy to what the
destination audience reaches, which is coarser than any reader's scope and reviewed as a
diff — so a digest is weaker and auditable rather than stronger and opaque.

Reversing toward a shared default is a one-way door for every message already posted under
it, since nothing recalls them.

## Revisit triggers

- A principal shape for a room's intersection appears that the grant tables and the
  diagnostic can both model, making a cohort-scoped answer computable and explainable.
- Epoch-keyed reachable-set caching becomes cheap enough that closure over a membership is
  no longer multiplicative.
- A hosting platform gains retention and recall controls the mirror can drive, which would
  move the end of the guarantee past publication.
- Askers are observed forwarding nearly every private answer into rooms, which would mean
  the default costs friction without changing outcomes.
