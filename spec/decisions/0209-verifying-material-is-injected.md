# 0209 — The exchange verifies an external assertion against material the operator injected, and makes no network call

**Status:** accepted 2026-09-18
**Decides:** `authority.exchange.interface.injected-material`

## Context

The exchange is the one place an external identity enters. It takes a verified assertion
from an identity provider, maps its claims onto a capability subject, draws grants from
the policy's role map, and mints a credential this engine verifies on its own. After that
moment the provider is not contacted again: every later read checks the capability
credential against pinned issuer keys, which is what keeps provider downtime off the read
path.

That leaves one question about the mint itself — how the exchange comes to hold the key
material that checks the assertion's signature. The conventional answer is discovery: the
exchange reads the provider's metadata document, follows it to a key-set endpoint,
fetches keys, caches them, and refreshes. That answer puts two network calls inside the
verification decision, and it puts them inside the component that turns an external claim
into local authority.

The exchange is provisioned per project as two files: a policy and the verifying material
beside it. The deployment target is a laptop as often as a server. A mint that requires
reachability to a provider's metadata host is a mint that fails on a plane, and a mint
whose correctness depends on what a remote document said at a particular moment is a mint
no test can reproduce without standing up that document.

Providers issue assertions under three shapes in practice: a shared secret, an RS256
public key, and a key-set document from which the right key is selected by the assertion
header's `kid`. All three are things an operator can obtain once and place in a file. The
third covers provider-side rotation without any fetch, because the document carries every
live key.

## Decision

Verifying material is injected by the operator — a shared secret, an RS256 public key in
PEM form, or a key-set document from which the key is selected by the assertion header's
`kid`. The exchange core performs no network call of any kind, so its verification
decision is a pure function of the assertion and the file beside its policy, and it runs
identically offline and in a test. An exchange configured with no verifying material
mints nothing and raises `ExchangeMaterialMissing`; a stale injected key fails closed at
the mint rather than healing itself.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Operator-injected material, three accepted shapes, no network call in the core** *(chosen)* | Verification is reproducible offline and in a test; the mint has no runtime dependency on the provider | The operator installs and rotates provider key material out of band, and a provider rotation the operator misses fails the mint closed |
| Provider discovery plus a key-set fetch inside the exchange | Provider rotation is picked up with no operator action | Loses on reproducibility: the verification decision depends on what a remote document returned at that instant, which no test reproduces without hosting one |
| Delegate verification to the provider's introspection endpoint | No key handling at all; the provider is authoritative about its own assertions | Loses on the same criterion and adds provider availability to every mint, plus a round trip on a path that is already the slowest part of a read |
| Accept a single key shape and require operators to convert | The smallest surface to implement and to reason about | Loses on provider coverage: the key-set shape is how rotation is published, and dropping it forces every rotation into an operator edit |

## Criteria

1. **Whether the verification decision is reproducible without a network.** *(decided
   it)* The others weigh operational convenience against surface area. This one decides
   what the exchange is: the component that converts an external claim into local
   authority. A decision that cannot be replayed from its inputs cannot be tested, cannot
   be audited after the fact, and cannot run where this engine is meant to run.
2. **Whether provider availability reaches the mint path.** Discovery and introspection
   both put it there; the read path is already protected, and the mint should not undo
   that.
3. **Coverage of how providers actually publish keys.** The three accepted shapes are
   what deployments hand an operator.
4. **Operator effort to track provider-side rotation.** Conceded, and the reason the
   key-set shape is accepted rather than only single keys.

## Consequences

The exchange becomes testable as a pure function: an assertion, a policy, a key, an
expected outcome. Offline and air-gapped deployments mint credentials the same way
connected ones do, and an incident at the provider cannot stop a project whose readers
already hold credentials.

The cost accepted is that provider key rotation is an operator obligation. A provider
that rotates and a project that does not update its injected material will fail every
mint, closed, until someone notices. The key-set shape narrows this — a document covering
several live keys survives a rotation into any key it already lists — but it does not
remove it, and the failure appears as new readers being unable to sign in while existing
credentials keep working, which delays detection.

Adding a fetch later is not expensive in code, but it is expensive in property: every
test written against a reproducible core would have to account for a decision that can
change without its inputs changing.

## Revisit triggers

- A provider in use rotates on a cadence short enough that operator-installed material is
  stale between scheduled updates.
- Mint failures attributable to stale injected material exceed mint failures from every
  other cause combined.
- A deployment appears where the operator cannot obtain the provider's key material out
  of band at all, leaving discovery as the only route to it.
