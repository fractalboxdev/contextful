# 0206 — Every derivation carries its own revocation identifier, so a delegated subtree is withdrawn without ending its root

**Status:** accepted 2026-09-18
**Decides:** `authority.revoke.interface.revocation-id`

## Context

Authority is delegated by derivation. A holder appends an attenuation block to a
credential it holds and signs it, producing a strictly narrower child, and that child is
derived again for a sub-agent, and again per query. The derivation is offline: no
issuer is contacted, and the party doing the deriving decides the child's shape. A
running agent loop therefore produces credentials the issuer has never seen and cannot
enumerate.

Withdrawal runs on two instruments. A short lifetime is the ordinary one — a credential
lapses before a list of withdrawn identifiers would have propagated. A denylist keyed on
the credential identifier, read at the verifying checkpoint, closes the window the
lifetime leaves open, and a scoped epoch invalidates a whole slice at once. Each of those
needs something to key on, and what it keys on determines the grain at which authority can
be taken back.

The event that forces the question is a delegate behaving badly: one sub-agent in a fleet
leaking its credential, or looping against a source it should not be touching. Everything
else derived from the same root is uninvolved, and in a local-first deployment those
siblings are doing the user's actual work.

The chain is also not knowable to the party doing the withdrawing. A checkpoint sees the
segments it was handed; an operator responding to an incident sees an audit record naming
a subject and a credential, not the full derivation history of every live descendant.
Anything keyed on the chain as a whole is keyed on something neither party holds.

## Decision

Every derivation carries a revocation identifier of its own, minted by the deriving
holder and covered by that hop's signature. The denylist is keyed on identifiers, so
naming one withdraws that credential and everything derived beneath it while leaving its
root and its siblings verifying. A checkpoint reads the identifier of every hop in the
chain it is presented with, and refuses when any of them appears on the list. The scoped
epoch stays available as the blunt instrument for the case where the root itself is
suspect.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One revocation identifier per derivation** *(chosen)* | Withdrawal at the grain of a single delegate; one misbehaving sub-agent is stopped without touching its siblings | The denylist carries an entry per withdrawn derivation rather than per root, and a checkpoint reads every hop's identifier |
| One identifier per root, inherited by every child | The smallest denylist and the cheapest check — one lookup per credential | Loses on blast radius: stopping one sub-agent stops every sibling derived from the same root, including the user's foreground work |
| A denylist keyed on the full derivation chain | Exact identification of one credential with no inheritance ambiguity | Loses on knowability: the chain is not available to the party performing the withdrawal, who holds a subject and an audit record, not a derivation history |
| No denylist; rely on the lifetime alone | Nothing to propagate, nothing to age out | Loses on response time: a compromised delegate keeps acting for the remainder of its lifetime, and the lifetime is chosen for usability, not for incident response |

## Criteria

1. **Blast radius of one compromised delegate.** *(decided it)* The others measure cost
   and precision; this one measures whether the instrument is usable during an incident
   at all. An operator who must end every sibling to stop one delegate will hesitate, and
   hesitation during a credential leak is the failure this instrument exists to prevent.
2. **Knowability to the withdrawing party.** A key that the responder cannot construct
   from what they hold is not a key.
3. **Denylist size and the checkpoint's per-request work.** Real, and it grows with
   delegation depth and fan-out.
4. **Time from decision to effect.** Every option here takes effect at the next
   checkpoint read; none distinguishes on this.

## Consequences

Incident response gains a scalpel: an audit record naming a credential is enough to stop
that credential and its descendants, and the response no longer costs the user their
running work. Delegation becomes safer to use deeply, since the depth does not increase
what a withdrawal has to destroy.

The cost accepted is denylist volume and per-request work. One entry per withdrawn
derivation rather than one per root means a fleet-wide incident writes many entries, and
a checkpoint reads every hop's identifier on every request rather than one. Entry ageing
bounds the first: an entry is dropped once the identifier it names can no longer verify
under any live key version. The second grows with chain length, and verification cost at
the deepest delegation the profile supports is unmeasured.

Reversing this is expensive: identifiers are covered by each hop's signature, so removing
them changes the signed envelope and invalidates every outstanding credential.

## Revisit triggers

- Denylist read latency becomes a measurable share of admission time at observed chain
  depths.
- Withdrawals in practice are observed to be root-wide almost always, making the
  per-derivation grain unused cost.
- A derivation shape appears where the deriving holder cannot mint an identifier the
  checkpoint can resolve — for instance a hop performed by a party with no clock and no
  source of uniqueness.
