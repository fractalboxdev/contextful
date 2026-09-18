# 0265 — A segment releases activation rather than data, and refuses below its audience floor rather than emitting a sentinel

**Status:** accepted 2026-09-18
**Decides:** `disclosure.release.refusal.audience-size`, `disclosure.release.refusal.member-readback`

## Context

Two derived results live under one contract and they are not the same object. A statistics
release publishes numbers over many contributors, and the whole release pipeline —
contribution capping, a distinct-contributor floor, noise calibrated to the bounded
sensitivity, a debited per-unit budget — exists to make a published number safe to read. A
lookalike segment publishes no numbers about its members at all. It resolves a candidate
pool against a seed centroid, and what leaves the engine is a segment identifier, a size,
coarse composition cells, and an activation handle the engine itself spends under a
capability scoped to one channel.

That difference changes what there is to protect. The release apparatus is a response to a
published figure that a consumer can difference against its own rows and public side
information. A segment publishes one figure — the size — and coarse cells already collapsed
at their own small-cell threshold. Noise over a size is noise over the one number the
requester needs to be correct in order to buy anything, and a per-unit budget debited
against members nobody can name pays for an accounting nobody can read.

The residual risk is a different shape. A member is not disclosed by a row leaving the
engine; a member is disclosed by the segment being small enough that its activation names
one person, or by its composition being read repeatedly across shifting parameters until
differencing isolates someone. A per-member score is the sharpest version of the second
path: a score is an ordering, and an ordering over candidates supports membership inference
that a set alone does not.

A floor over audience size is therefore not a small-cell rule wearing a different number.
A small cell is a valid answer whose precision is too fine to publish, and collapsing it
into a sentinel is an honest, useful answer. An audience of four is not an answer of low
precision; it is a way of pointing at four people. There is no degraded form of it worth
handing back.

## Decision

A lookalike segment releases a segment identifier, its size, coarse composition cells, and
an activation handle. Member rows and per-member scores stay inside the engine, and
readback on a segment is `stats_only`: a request for its members raises
`DisclosureSegmentMemberReadback`. A segment resolving below its declared audience floor
raises `DisclosureAudienceBelowFloor` and produces no segment, where a composition cell
under its own threshold collapses into a sentinel and the segment continues. The
release pipeline's noise calibration and per-unit budget attach to a statistics release and
not to a segment build, and a size-gated opaque segment is described as exactly that rather
than as a differentially private one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Activation-only output, refusal below the floor** *(chosen)* | Nothing per-individual leaves the engine, so there is no disclosure for the release apparatus to bound. An under-floor build fails loudly at the moment the requester would otherwise target a handful of people. | The requester cannot audit the segment it activates against, and a tenant probing with repeated narrow rebuilds still learns coarse shape. |
| Applying the noise-and-budget apparatus to the segment path | One pipeline for both derived results; a uniform vocabulary of guarantees. | Lost on the disclosure criterion: it prices in machinery for a per-individual disclosure the output shape already prevents, degrades targeting precision for no gain, and buys a stronger-sounding description than the mechanism delivers. |
| Releasing the member list under a contractual restriction | Full auditability of who was targeted; the requester can reconcile against its own base. | Lost on the same criterion: a member list is a per-individual disclosure under any reading, and a contract is enforced outside the engine while the rows are already gone. |
| Releasing per-member scores without the identities | Lets the requester tune a threshold itself rather than restating the target size. | Lost on the same criterion: a score vector supports membership inference the segment set alone does not, because an ordering is evidence about each position in it. |
| Emitting a sentinel below the audience floor | Consistent with how a small composition cell behaves; the caller gets a shaped answer. | Lost on answer-shape honesty: a sentinel says "this exists and is too small to publish", which is a valid answer, and a too-small audience is a re-identification rather than a coarse answer. |

## Criteria

1. **Per-individual disclosure** — whether anything leaving the engine is about one
   identifiable member. **This criterion decided.** Every other property here is a way of
   bounding a disclosure; if no per-individual disclosure occurs, there is nothing to bound,
   and the apparatus that would bound it is pure cost. It outranks the others because it
   changes which question is being asked rather than answering it better.
2. **Honesty of the answer shape** — whether a returned value means what its shape claims.
   A sentinel claims "too small to publish precisely"; a four-person audience is not that.
3. **Strength of the description** — whether the word attached to a release matches the
   mechanism behind it. Bounded, differentially private and size-gated-opaque are three
   different claims.
4. **Targeting utility** — whether the requester can act on what comes back. Noise over an
   audience size degrades the one figure the output exists to carry.
5. **Auditability by the requester** — whether the party activating a segment can check it.
   The chosen option is the worst on this criterion.

## Consequences

The segment path carries no differential-privacy dependency, no per-unit budget accounting,
and no calibration parameters, so the machinery that does exist is concentrated on the
statistics path where a published figure is the thing at risk. A reader of the contract can
tell which guarantee a given release carries by reading which mechanisms ran, and the three
descriptions stay distinguishable rather than collapsing into one marketing word.

The cost accepted: the consumer cannot audit the segment it activates against. It receives a
size and coarse cells and has to trust the resolution that produced them, which means a
targeting error inside the engine is invisible to the party paying for the campaign. A
second cost is stated and undefended: a tenant issuing repeated narrow rebuilds across
shifting parameters still learns coarse shape, and coarse composition cells plus rate-limited
rebuilds are mitigations rather than a bound. The size of that residual is unmeasured — no
figure exists for how many rebuilds isolate a member under the working floor and cell
threshold.

Reversing the refusal into a sentinel is cheap. Reversing the output shape is not: a
deployment that has advertised activation-only handles cannot begin returning members
without changing what every prior segment meant.

## Revisit triggers

- A deployment demonstrates a targeting workflow that a size and coarse cells cannot serve,
  and the gap is in the output shape rather than in the floor's value.
- Repeated-rebuild differencing is measured and the number of rebuilds needed to isolate a
  member falls near what a rate limit actually permits.
- A release on the segment path begins publishing a per-member figure of any kind, at which
  point the noise-and-budget apparatus stops being redundant.
