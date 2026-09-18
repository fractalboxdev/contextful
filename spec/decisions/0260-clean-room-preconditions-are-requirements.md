# 0260 — The three cross-owner protections are deployment requirements, and the setting declines a store that meets fewer

**Status:** accepted 2026-09-18
**Decides:** `disclosure.set-mode.refusal.clean-room`

## Context

A clean room is a query pattern across a trust boundary between distinct data owners. The
boundary is what defines it — not the fact that the result is derived — so every mechanism
in the setting exists to make one owner's misbehavior detectable by the other.

Three mechanisms carry that load, and each closes a distinct path. Per-tenant signed
manifest entries hanging in a hash tree let a reader verify the path from its own subtree
to the root, so a co-tenant rewriting the top-level manifest is caught by verification.
Per-tenant write prefixes, enforced by the object store's own access policy, stop one owner
writing over another's bytes. Per-tenant signing keys held in each owner's own key store
mean a forged cross-owner grant has no signer, and the escrow carrying peppers between the
pair holds no signing material at all.

Two of the three are not enforceable from inside the engine. A write prefix is a property of
the store's access policy; an engine can decline to write outside its prefix, but only the
store can stop a co-tenant who declines to decline. Key custody is the same shape.

That leaves the decision: are these documented trust assumptions an operator is told about,
or preconditions the setting checks before it operates? The difference shows up in what a
co-tenant does undetected when one is missing. Without subtree signatures they rewrite the
top-level manifest silently. Without prefix policy they overwrite another owner's bytes.
Without separate key custody a grant appears that nobody can be shown to have signed. Each
failure arrives as data corruption or as a forged grant, not as an error.

## Decision

A cross-owner deployment carries per-tenant signed manifest entries, per-tenant write
prefixes enforced by the object store's own access policy, and per-tenant signing keys held
in each owner's own key store, as deployment requirements rather than trust assumptions. A
store meeting fewer than all three raises `DisclosureCleanRoomPreconditionUnmet` and the
setting declines to operate against it. The cross-owner profile validates the configuration
ahead of any write that crosses a prefix boundary.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **All three checked as preconditions; the setting declines a store meeting fewer** *(chosen)* | Every path a co-tenant could take silently is closed before the first cross-prefix write. | A clean room cannot be stood up against an object store whose access policy cannot express per-prefix writes, which rules out some hosted stores outright. |
| Document the three as operator responsibilities | Works against any store, and puts the choice with the party who owns the risk. | Loses on detectability: the failure is silent and arrives as data corruption or a forged grant, so the responsibility is discharged or not with no signal either way. |
| Enforce them inside the engine alone | No dependency on store capabilities; one implementation. | Loses on enforceability: a write prefix is enforceable only by the store's own access policy, and an engine-side check binds the well-behaved party and nobody else. |
| Allow a degraded mode meeting two of the three | Most stores qualify; the common deployment proceeds. | Loses on coverage: each protection closes a distinct path and none substitutes for another, so a two-of-three deployment is fully exposed on the third. |
| Check at first cross-prefix write rather than at configuration | Nothing blocks a single-owner configuration that never crosses the boundary. | Loses on timing: the first crossing is already a write, and a failure there is discovered with data in flight rather than at setup. |

## Criteria

1. **What a co-tenant does undetected when a protection is absent.** *This criterion
   decides.* The setting's entire premise is mutual distrust between owners, and a
   protection whose absence is invisible converts that premise into an assumption the
   parties did not agree to and cannot check.
2. **Whether the protection is enforceable by the party that needs it.** Two of the three
   live in the store, so an engine-side rule is not the same rule.
3. **Whether the protections are independent.** They are, which is why two of three is not
   a partial guarantee.
4. **Range of stores a deployment can use.** Given up.

## Consequences

Store selection becomes part of the setting's design rather than an operational detail. An
object store whose access policy cannot express per-prefix writes is out, and that
eliminates some hosted offerings entirely — a real constraint discovered at procurement
rather than at integration.

The accepted cost is that narrowing. An operator with an otherwise suitable store, a
willing counterparty and a genuine use case is told no, and the answer is a property of
their infrastructure they may not be able to change.

What gets easier is the conversation between owners. The three requirements are checkable
facts rather than assurances, so a party joining a clean room verifies the arrangement
instead of trusting a description of it, and the cross-owner profile reports which
requirement is unmet rather than failing abstractly.

Reversing toward documented assumptions is cheap to code and unwinds the guarantee for every
deployment at once, with no record of which ones were relying on it.

## Revisit triggers

- An object store family gains per-prefix write policy, which would move deployments
  currently excluded into range.
- A protection appears that subsumes another — for example, store-enforced per-writer
  attestation covering both prefix and manifest integrity — which would make three the
  wrong count.
- Deployments are observed standing up clean rooms outside this setting to work around the
  refusal, which would mean the requirement is pushing risk out of view rather than
  removing it.
