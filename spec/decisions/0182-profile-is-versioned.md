# 0182 — A checkpoint refuses a profile version it does not implement, and widening the profile mints a new version

**Status:** accepted 2026-09-18
**Decides:** `authority.profile.refusal.profile-version`

## Context

Delegated authority travels as inert data read against a profile. The profile names the
exact block versions, predicates, rules and restriction tuples the engine admits, and a
credential carrying anything outside that set is refused rather than treated as
unconstrained. The profile is therefore the whole of what a checkpoint knows about how
to read a credential.

Checkpoints are not upgraded together. A primary, a replica, an embedded face in a
consumer's service and an operator's command line each hold their own build, and each
verifies offline — there is no round trip to an issuer at admission, and no place for a
checkpoint to ask what a credential means. Whatever a checkpoint knows about the profile
is compiled into it.

That makes profile change a version-skew problem with a direction. If the profile is
edited in place to admit more, a checkpoint on the old build reads a credential written
against the new profile and has two ways to be wrong: refuse an element it should admit,
or admit an element whose meaning it does not implement. The second is the dangerous
one, because profile elements are predominantly narrowings — an element an old build
does not understand is one it also does not enforce.

## Decision

A credential names the profile version it was written against. A checkpoint reading a
version it does not implement raises `ProfileVersionUnsupported` and admits nothing.
Widening the profile mints a new version; an existing version is never relaxed in place.
A checkpoint therefore either implements the exact semantics a credential was written
under, or refuses it.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Version the profile; refuse an unimplemented version; widen by minting a new one** *(chosen)* | A checkpoint's enforcement of a credential is exactly the enforcement the issuer intended, or there is no admission at all. | Every checkpoint is upgraded before the first credential under a new version is minted. A missed checkpoint refuses live traffic. |
| Relax an existing version in place | No rollout ordering to manage; one version number forever. | Loses on silent under-enforcement: an old build admits a credential written against semantics it does not implement, and the narrowings it cannot read are the ones it drops. |
| Negotiate capability between holder and checkpoint | The holder presents the strongest credential the checkpoint can enforce. | Loses on the offline requirement: verification runs with no round trip, so there is no exchange in which to negotiate. |
| Admit an unknown version and enforce the intersection the checkpoint understands | Availability across skew. | Loses on under-enforcement in the same way as relaxing in place, and adds an authority value no issuer ever authored. |
| Treat the version as advisory metadata | Trivial to implement. | Loses on determinism: two checkpoints admit the same bytes to different reach, with nothing recorded saying which one acted. |

## Criteria

1. **Under-enforcement under version skew** — whether an older checkpoint can admit a
   credential written against newer semantics and enforce less than the issuer wrote.
   *This criterion decides.* Every other option here trades a failure that is visible —
   a refusal, a deployment ordering — for one that is not, and an invisible
   under-enforcement of a delegation profile leaves no artifact to find it by.
2. **Availability under skew** — whether traffic continues while builds differ.
3. **Offline verifiability** — whether the checkpoint needs a round trip. This is a
   hard requirement, not a preference, and it eliminates negotiation outright.
4. **Rollout cost** — the operational work a profile change requires.

## Consequences

Rolling a profile change becomes an ordered operation: every checkpoint that will see
credentials under the new version is upgraded first, and only then does the issuer begin
minting under it. Mixed fleets stay correct as long as the issuer stays behind the
slowest checkpoint.

Debugging gets easier: a refusal names the version, so skew presents as a specific,
searchable identifier rather than as a grant that appears to work and quietly reaches
further than it should.

The accepted cost is the upgrade ordering, and its sharp edge — a checkpoint that is
missed does not degrade, it refuses. A forgotten replica takes its traffic to zero the
moment the issuer rolls forward.

Reversing this is expensive: once holders and consumers rely on a version stamp meaning
exactly one set of semantics, relaxing a version in place would invalidate every
assumption built on that stamp, including any audit that reconstructed a past decision
from it.

## Revisit triggers

- A checkpoint gains a way to fetch profile semantics at admission time without a
  network dependency on the hot path — a signed profile document distributed with the
  credential, say — which would make negotiation expressible for the first time.
- The rate of profile change rises to where ordered fleet upgrades dominate release
  cost, making the rollout-cost criterion competitive with under-enforcement.
- A profile change that strictly narrows is needed urgently across a fleet that cannot
  be upgraded in time, forcing the question of whether narrowing-only edits deserve a
  path that widening does not.
