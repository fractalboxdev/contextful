# 0177 — Delegated authority rides an existing attenuable credential library under a profile this engine owns

**Status:** accepted 2026-09-18
**Decides:** `authority.profile.interface.delegation-profile`

## Context

The delegation model the engine needs is specific. A holder narrows what it holds with no
round trip to an issuer, by appending a block and signing over what it received. A
checkpoint verifies the whole chain holding public key material and no secret, with no
call to an issuer or a policy service. Truncating a chain back to a broader prefix yields
nothing verifiable. Those are the properties every local-first deployment rests on: the
read path keeps working while the identity provider is unreachable, and a compromised
checkpoint mints nothing.

Building that is cryptographic work — chain signatures, an advancing ephemeral proof, a
serialization every verifier agrees on byte for byte — and it is the class of work where
a subtle error is both easy to make and invisible until it is exploited. Existing
libraries implement exactly this shape and have been reviewed by people who do that for a
living.

What those libraries also carry is a language. The credential is not a fixed set of
fields; it is blocks of facts, checks and rules evaluated by a datalog-flavored engine
that admits third-party blocks, external functions, recursion and regular-expression
predicates. Adopting the library's full language means every credential the engine admits
is an input to an evaluator whose cost and whose reachable facts are decided by the party
holding the credential.

Those are separable. Serialization, signatures, chaining and the evaluator are the
library's. Which facts exist, which checks are recognized, which restriction tuples mean
anything, and what any of it maps onto in terms of the rows a caller reaches — those are
statements about this engine and exist nowhere upstream.

## Decision

Delegated authority travels as an attenuable, chain-signed credential from an existing
library, read against a versioned profile this engine owns. The profile names the exact
facts, checks and restriction tuples the engine admits, and its mapping onto authority is
owned here. The library owns serialization, signatures, block chaining and evaluation. An
element the profile does not name is a refusal rather than an unconstrained admission.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The library's format and evaluator, under an owned profile** *(chosen)* | Reviewed cryptography for the chain, and a bounded admitted vocabulary the engine can reason about and model formally | The wire format tracks an upstream project, and a breaking change there reaches every credential in circulation |
| A hand-rolled credential format | Exactly the fields needed, no upstream dependency, no unused language to bound | Lost on review cost: chain signatures and an advancing proof would be reviewed by nobody outside this project, and the failure mode is silent forgery rather than a visible bug |
| The library and its full language, unprofiled | Nothing to author, every upstream feature available immediately | Lost on bounding: third-party blocks, external functions, recursion and regex predicates make evaluation cost and reachable facts holder-controlled, and an unnamed element admits as unconstrained |
| A bearer token plus a server-side policy lookup | Familiar, no attenuation logic at all, policy changes take effect instantly | Lost on read-path independence: every admission becomes a call to a policy service, so the read path fails when that service does, and offline narrowing disappears |
| A signed claims envelope with fixed fields and no chaining | Simple to verify, easy to bound, no language at all | Lost on offline derivation: a holder cannot narrow without asking the issuer, so sub-agent fan-out takes a mint per hop |

## Criteria

1. **Cryptographic review** — how many competent eyes have examined the chain
   construction the security rests on.
2. **Bounded admitted vocabulary** — whether the set of things a credential can say is
   known to the engine and small enough to reason about.
3. **Read-path independence** — whether admission requires reaching any service.
4. **Offline derivation** — whether a holder narrows without a round trip.
5. **Upstream coupling** — what an upstream change costs.

Criterion 2 decided it, over criterion 5. Criteria 1, 3 and 4 already eliminate the
hand-rolled format, the policy lookup and the fixed envelope, which leaves the library
adopted either way — so the live question is how much of it. Bounding the vocabulary
outranks the coupling it adds because the profile is what every later authority statement
is written against: the refusals for unrecognized elements, reserved facts and evaluator
budget all exist only as statements about a named set, and the formal model's theorems
quantify over that set. An unprofiled adoption leaves nothing to state them about.

## Consequences

The chain's cryptography is somebody else's reviewed work and improves when theirs does.
The engine's authority surface is a finite list of named elements, which is what makes
the mapping onto effective authority modelable and what lets a checkpoint refuse an
unknown element instead of guessing at it. One profile module, compiled natively and to
WebAssembly, is what a gateway and the engine both execute, so the two cannot drift.

The cost accepted is coupling to one library's serialization and signature scheme. A
breaking change upstream reaches the wire format, and the engine's credentials are not
interchangeable with anything that has not adopted the same library. The profile also has
to be maintained alongside the library: an upstream release adding an element makes every
credential using it refuse here until the profile is versioned to name it, which reads to
a holder as this engine being behind.

Reversing the format is expensive — every credential in circulation is in it, and
introspection, attenuation and the formal model all name its elements.

## Revisit triggers

- The upstream library changes its wire format or its evaluator semantics in a way the
  profile cannot absorb by versioning.
- A deployment needs a credential that interoperates with a format this engine does not
  speak, which weighs interoperability against criterion 5.
- The profile's named element set grows large enough that bounding it stops being what
  makes the mapping reasonable to model.
