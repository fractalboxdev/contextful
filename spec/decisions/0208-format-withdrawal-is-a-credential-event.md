# 0208 — Withdrawing a credential format runs through revocation, expiry or key rotation, never through a flag

**Status:** accepted 2026-09-18
**Decides:** `authority.revoke.workflow.format-withdrawal`

## Context

The wire format is version-tagged: a version segment, base64url claims, an issuer
signature, and, once derived, an attenuation segment and a holder signature. The version
segment exists so a second format can be introduced without ambiguity, which means the
first format eventually stops being accepted. Ending acceptance of a format is a
withdrawal of authority — every outstanding credential in that format is authority the
project has decided to stop honoring.

The instruments for withdrawing authority already exist and already have the properties
this needs. A credential lapses on its own lifetime, bounded by the persisted issuance
ceiling. A denylist entry names an identifier. A scoped epoch invalidates a slice at
once. An issuer key version is retired and stops verifying after its grace window. Each
of these has a defined instant at which it takes effect, and each is declared rather than
configured.

The tempting alternative is a deployment flag: turn old-format acceptance off, watch for
breakage, turn it back on. That is how a compatibility window is usually managed, and it
is why acceptance can come back without anyone declaring it. A rollback of the
deployment — performed for an unrelated reason, by someone who never read the
withdrawal — restores acceptance of a format the project decided to stop trusting, and
nothing in the system records that this happened.

The other pressure is sequencing. Credentials in the old format are in flight at the
moment of the decision, held by agents and embedded applications that will present them
until they lapse. A cutover that ignores them refuses credentials that were valid when
minted, which is the same holder-visible failure a short rotation grace produces.

## Decision

Withdrawing a credential format runs through revocation, genuine expiry or issuer-key
rotation under a declared compatibility policy. Every admission path stops admitting the
withdrawn format at the cutover instant the policy names, and a credential in a withdrawn
format raises `CredentialFormatWithdrawn` wherever it is presented. Restoring acceptance
takes an explicit declaration; no rollback, flag flip or configuration edit brings a
withdrawn format back.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Withdrawal through revocation, expiry or key rotation under a declared policy** *(chosen)* | One recorded cutover instant; acceptance cannot return without a declaration; outstanding credentials are sequenced out rather than cut off | The cutover instant is bounded by the longest lifetime in flight, so a withdrawal cannot be immediate under the ordinary instruments |
| A quiet dual-accept window, closed when breakage stops | Simple to operate; no policy to write | Loses on the recorded-end criterion: nothing states when acceptance ends, so no party can answer whether a given credential was honored |
| A deployment flag controlling old-format acceptance | Fast to flip in both directions during a cutover | Loses on silent restoration: a rollback performed for an unrelated reason restores the old format with no declaration, and the system holds no record that it did |
| Refuse the old format at a fixed date with no sequencing | An unambiguous instant, trivially implemented | Loses on the holder-visible failure: credentials valid on their own terms stop verifying, which is the failure the rotation grace exists to prevent |

## Criteria

1. **Whether a withdrawn format can be re-accepted without a declaration.** *(decided
   it)* The other criteria concern operational cost and timing, which are recoverable. A
   format is withdrawn because it is no longer trusted; an arrangement in which trust
   returns as a side effect of an unrelated deployment action means the withdrawal never
   actually happened, and no audit of the period can say otherwise.
2. **Whether the end of acceptance has a recorded instant.** A window with no stated
   close cannot be reasoned about after the fact.
3. **Whether credentials valid when minted keep verifying to their expiry.** Met by
   running the withdrawal on the same instruments as rotation.
4. **How quickly a withdrawal can take effect.** Conceded under the ordinary path, and
   answered separately: a format withdrawn because it is unsafe runs on the scoped epoch,
   which is immediate by design.

## Consequences

Format evolution inherits everything the withdrawal instruments already provide: a stated
cutover instant, a refusal that names itself, and an interval in which outstanding
credentials drain rather than break. An auditor can answer which format was admitted at
any instant by reading declarations rather than deployment history.

The cost accepted is that a withdrawal must be sequenced against outstanding credentials,
so under the ordinary instruments the cutover instant is bounded by the longest lifetime
in flight — up to the persisted issuance ceiling. A project wanting a faster cutover
lowers that ceiling first, or reaches for the scoped epoch and accepts that it ends live
sessions along with the format.

The declaration itself becomes a durable artifact that has to be maintained, and a
mistaken withdrawal is undone only by a second declaration, not by reverting a change.

## Revisit triggers

- A format is found unsafe in a way that makes any drain interval unacceptable, and the
  scoped epoch's collateral cost is judged worse than a flagged emergency cutoff.
- More than one format withdrawal occurs within a single issuance-ceiling interval,
  making the sequencing constraints of consecutive withdrawals overlap.
- A deployment shape appears in which admission paths cannot all read the compatibility
  policy, so the cutover instant is not simultaneous across hops.
