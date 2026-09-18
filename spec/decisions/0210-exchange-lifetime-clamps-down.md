# 0210 — A configured exchange lifetime clamps down to the ceiling and never up

**Status:** accepted 2026-09-18
**Decides:** `authority.exchange.limit.minted-lifetime-ceiling`

## Context

A project persists an issuance policy naming the longest lifetime any mint path produces.
Every path respects it, but they do not respect it the same way. An explicitly requested
lifetime above the ceiling is refused outright and never quietly shortened, because a
caller asking for eight hours and silently receiving one has been told something false
about the credential it holds. That path has a caller who can read an error and change
what it asked for.

The exchange path has no such caller. Its lifetime comes from `ttl_secs` in a policy file
an operator wrote, defaulting to 900 s when absent, and the exchange runs on every request
an embedding application makes on behalf of a signed-in reader. The value is a
configuration fact, not a per-request argument, and the party who could correct it is not
present when the mint happens.

That changes what a refusal does. A policy file with a mistyped `ttl_secs` is a file an
operator edits for unrelated reasons — adding a role mapping, changing an audience — and
a hard refusal at load turns any such edit into an exchange that does not start. The
readers affected are people trying to sign in to an application, and the failure is total
rather than reduced.

A minted credential also sits under a second constraint that is not a policy at all. It
descends from authority the project holds, and a derived credential is never longer-lived
than what it descends from. Honoring a configured value above the ceiling would produce a
credential the mint path has no authority to produce.

## Decision

A configured minted lifetime clamps to a ceiling of 3600 s. The clamp reduces and never
extends: a policy naming a larger number mints at the ceiling, and a policy naming a
smaller one mints at the smaller number. Every misconfiguration on this path therefore
fails toward less access — a shorter session — rather than toward a longer-lived
credential than the project sanctions. A caller reads the effective lifetime off the
credential it received.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Clamp down to the ceiling, never up** *(chosen)* | Every configuration mistake on this path lowers privilege; an unrelated policy edit cannot take sign-in offline | An operator who genuinely wants a longer session receives silent truncation rather than an error, and learns of it only by reading the minted credential |
| Refuse the policy at load when `ttl_secs` exceeds the ceiling | The mistake is named at the moment it is introduced | Loses on failure direction and on blast radius: the mint path already enforces the persisted ceiling, so the refusal adds no safety, and it turns an unrelated policy edit into a total sign-in outage |
| Honor the configured value | The operator's stated intent is respected exactly | Loses on descent: a minted credential cannot outlive the authority it derives from, so this produces a credential the mint path has no standing to issue |
| Clamp and emit a warning at each mint | Truncation becomes visible without failing anything | Loses on signal: the warning fires on every request for as long as the policy stands, which is the shape operators filter out |

## Criteria

1. **Direction of a configuration mistake.** *(decided it)* The others weigh
   discoverability against availability. This one is the only asymmetric property
   available: clamping in one direction means no typo, no stale file and no copied
   template can ever produce more access than intended. Discoverability failures cost an
   operator time; direction failures cost a project its lifetime bound.
2. **Blast radius of an unrelated edit to the same file.** The policy carries role
   mappings and audiences that change often; a lifetime check that refuses the whole file
   couples them.
3. **Whether the minted credential can exceed the authority it descends from.** A hard
   constraint, not a preference, and it eliminates the honor-the-value option outright.
4. **Whether the operator can discover the truncation.** Conceded as weak: the effective
   lifetime is legible on the minted credential, and nothing announces it.

## Consequences

An operator can edit the exchange policy without the risk that a lifetime mistake takes
sign-in down, and every mistake that does reach the mint produces shorter sessions rather
than longer ones. The exchange path and the explicit-request path differ deliberately,
and the difference tracks whether a caller is present to read a refusal.

The cost accepted is a silent truncation. An operator who sets `ttl_secs` to four hours
in good faith gets one hour, with nothing in the exchange's behavior pointing at the
policy line responsible. They notice through readers re-authenticating sooner than
expected, and they confirm it by reading the effective lifetime off a minted credential.
That is a worse discovery path than an error, and it is accepted because the alternative
error lands on the wrong party at the wrong moment.

Raising the ceiling later is cheap; lowering it silently shortens every live session
minted through the exchange.

## Revisit triggers

- Support traffic shows operators repeatedly misreading the effective session length,
  indicating the truncation is undiscoverable in practice rather than merely quiet.
- A legitimate embedding use appears that needs sessions longer than 3600 s and cannot be
  served by re-exchange on the application's side.
- The exchange gains a validation surface an operator runs deliberately, at which point a
  refusal can be delivered without coupling it to a file edit.
