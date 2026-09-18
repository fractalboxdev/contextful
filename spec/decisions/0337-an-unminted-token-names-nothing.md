# 0337 — A token with no row behind it refuses, and allocates nothing

**Status:** accepted 2026-09-18
**Decides:** `run.suspend.refusal.unknown-token`

## Context

An awakeable suspends a run durably. The engine mints an opaque single-use token, persists a
pending row beside the journal, and an external party resumes the run by posting that token
back. The resume payload is written as the awaited step's output under a key derived from the
token, so the token is simultaneously the name of a suspension, the address of a journal entry
and the credential that resumes it.

The resume route is reachable by anyone the face admits, and the party holding a token is
outside the engine's control: a browser completing an authorization redirect, a vendor's
webhook, a tool posting a result. Authentication runs ahead of the registry, so an
unauthorized callback leaves the suspension pending — but an admitted caller can still present
any string it likes, and a string is cheap to produce.

The registry therefore meets tokens that name no row on three ordinary paths. A callback fires
against the wrong deployment, so the token is genuine somewhere else. A party retries long
after the run closed and the row was reaped. A caller mistypes, or probes. What the registry
does with the string decides two separate things: what state a presented string can bring into
existence, and what an arbitrary string learns.

The first is the sharper one. A registry that creates a pending row for an unrecognized token
lets the caller author the journal key that row resolves under. A payload posted ahead of a
mint then sits in the journal waiting for the engine to suspend under the same key, and the
step reads a value nobody's run produced. The exactly-once contract holds — the first write
under a key is the value — which is precisely what makes the pre-seeded write durable.

## Decision

A token with no row behind it raises `AwakeableUnknown`, answering `404` on the resume route.
No row is created and no journal key is written, so an arbitrary string allocates no state. The
refusal carries the token's absence and nothing else, so a caller holding one token learns
nothing about any other suspension, and a closed suspension answers by its own name rather than
by this one.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse by name, create nothing** *(chosen)* | Every pending row traces to a mint the engine performed, and the journal key of a suspension is derived from a token the engine chose. | An honest caller whose suspension was reaped and one holding a fabricated string receive the same answer, so a lost row is diagnosed from the run record rather than from the route. |
| Create a pending row on first resolution | The route is order-independent: a callback arriving before the engine suspends is held rather than lost. | Lost on who authors state: a presented string becomes a durable row and a journal key, so a caller pre-seeds the value a later step reads, and the exactly-once contract makes that write permanent. |
| Answer success as though the token had resolved | No error arm at all, and a retrying party stops retrying. | Loses on legibility: the external side believes its payload landed and acts on that belief, while nothing in the engine holds it. |
| Answer the closed-suspension refusal for every token that names no live row | An unknown token and a lapsed one are indistinguishable, so probing learns less. | Loses on diagnosability: a party that missed a deadline and a party holding a wrong string receive one answer, and the two have opposite remedies. |
| Report whether the token was ever minted | A caller can tell a typo from a reap without consulting an operator. | Loses on what a string learns: the route becomes a membership test over minted tokens, answerable by anyone the face admits. |

## Criteria

1. **Who authors durable state** — whether a value a caller presents can bring a row and a
   journal key into existence. *This criterion decides.*
2. **What an arbitrary string learns** — whether the route answers questions about suspensions
   the caller does not hold.
3. **Legibility to the external party** — whether the answer matches what actually happened.
4. **Diagnosability** — whether a caller can distinguish the failure modes it can act on.

Criterion 1 decides. A token is the engine's own mint, and everything downstream of it — the
pending row, the deterministic key, the step's recorded output — is addressed by it. Letting a
presented string mint that address moves the authorship of a journal entry outside the engine,
and the journal's own guarantee then preserves the result. Criterion 2 eliminates reporting
more than absence; criterion 4 is what keeps the closed suspension answering by its own name
rather than collapsing every miss into one reply.

## Consequences

The registry's population equals the set of suspensions the engine minted, which is what lets a
count of pending rows be read as a count of waiting runs, and what keeps the resume route from
being a write path for anyone who can reach it. Probing it yields one bit per guess against an
opaque token, and the guess costs the engine a lookup and nothing more.

The cost accepted is that a genuine callback can be refused with no path to recovery. A party
whose suspension was reaped, or whose callback reached a deployment that never minted its
token, is told only that nothing here awaits it — and the run it was resuming is found through
the run record instead. Where a suspension sits behind a human approval gate, that means the
gate is re-opened by firing again rather than by re-posting.

Ordering is the second cost. A vendor fast enough to call back before the engine has persisted
the pending row is refused, so the mint is ordered ahead of handing the token out, and a source
that cannot be held to that order carries its own outbox.

## Revisit triggers

- A vendor class appears whose callbacks routinely precede the mint, making an
  arrival-before-suspension buffer cheaper than ordering the handout.
- Reaping closes rows early enough that honest late callbacks become common, which would argue
  for retaining a tombstone per token and answering by its name.
- The resume route gains a caller identity strong enough to scope a membership answer to the
  party that received the token, which would reopen what the refusal reports.
