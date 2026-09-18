# 0142 — Declaring any header selects the hardened client and the cleartext refusal

**Status:** accepted 2026-09-18
**Decides:** `secret.attach.refusal.cleartext-endpoint`

## Context

Two protections apply to a source that carries credential material outward. The system proxy
is bypassed, so credential traffic does not leave the machine through a component chosen by
ambient configuration. And a cleartext endpoint is refused, so material is not sent in the
clear.

Both need a test for "does this source carry material". The obvious test reads the value: a
header whose value hydrates from a `secret://` reference is sensitive, everything else is
not. It is precise, it is cheap, and it misses the case that matters most. An author who
pastes a token directly into a header value — during development, in a hurry, or because the
reference grammar was not obvious — has written a source that carries a credential and whose
values contain no reference spelling. That source keeps the proxy and passes the cleartext
check, and the pasted token is forwarded verbatim. The value-spelling test is exactly
inverted against the least careful author.

Widening the value test does not fix it. Inspecting values heuristically for token shapes is
the same guess one layer down: entropy thresholds and prefix patterns produce false negatives
that nobody reviews, and the failure is silent in the same direction.

The other end — hardening every source unconditionally — reaches the pasted token and
everything else, at a price paid by a different deployment. A machine whose sole egress is a
corporate proxy loses every pull, including sources that carry nothing at all.

## Decision

Declaring any header selects the hardened client and bypasses the system proxy; a source
declaring none keeps the proxy. The test reads header presence rather than the spelling of a
value, covering a token pasted inline as well as a hydrated one. A header whose value hydrates
from a reference is marked sensitive and raises `SecretCleartextEndpoint` against a cleartext
endpoint, with loopback exempt in its IPv4 and IPv6 forms.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Key hardening on header presence** *(chosen)* | A pasted token is covered as fully as a hydrated one, and the test is a structural fact nobody can spell their way past | The test is coarse: a source declaring only `Accept` loses the proxy and fails as a connect error at read time rather than as a refusal at build |
| Key on the `secret://` spelling | Precise, and it names exactly the values the host hydrates | Lost on the pasted-token case, which is the one case where an author is least careful and the material is most exposed |
| Inspect header values heuristically | Catches pasted tokens without hardening unrelated sources | Lost on the same case by a different route: it is a guess one layer down, with false negatives nobody reviewed and no signal when it misses |
| Harden every source, including the proxy bypass | Nothing carrying material is missed, under any spelling | Lost on reachability: a deployment whose sole egress is a corporate proxy loses every pull, including sources that carry no material at all |
| Let the operator mark a source as sensitive | Exact, and the operator knows best | Lost on the same failure mode as the spelling test: the author who pastes a token is the author who does not set the flag |

## Criteria

1. **What the test misses** — which credential-bearing sources fall outside it, and how
   silently. *This is the criterion that decided it.* A token pasted inline is forwarded
   verbatim on a cross-host hop, so a test reading the spelling of a value misses exactly the
   case that leaks, and misses it without any diagnostic.
2. **Reachability** — whether a deployment can pull at all. It is what rules out hardening
   everything, and it is the only reason a source with no headers keeps the proxy.
3. **Predictability** — whether an author can tell from the declaration which client a source
   gets. Header presence is readable at a glance; a heuristic is not.
4. **Precision** — how many sources are hardened that did not need it. The chosen option loses
   here and accepts it.

## Consequences

Every source that carries material — hydrated or pasted — gets the hardened client, and the
determination is a structural property of the declaration rather than a judgment about a
value. An author reading a source knows which client it will use by looking at whether an
`[attach]` block or any header entry exists.

The accepted cost: the test is coarse. A source declaring only `Accept` carries nothing and is
hardened anyway, and on a proxy-only network it fails as a connect error at read time rather
than as a refusal at build. That is the worst shape of failure this decision produces — late,
and named at the transport rather than at the cause — and an operator diagnosing it has to
know that a header entry is what removed the proxy.

The cleartext refusal keys differently, on a value hydrating from a reference, and is
therefore narrower than the proxy bypass. A pasted token against a cleartext endpoint is not
refused, which is a gap this decision does not close: the pasted case is covered for proxy
exposure and not for transport exposure.

## Revisit triggers

- Sources declaring only non-credential headers are observed failing on proxy-only networks
  often enough that the coarseness is a routine operational cost rather than an edge.
- The declaration grammar gains a structural place for non-credential headers distinct from
  `[attach]`, at which point presence-in-`[attach]` is a sharper test than presence-of-any-header.
- A mechanism appears that reliably detects credential material in a value without heuristics
  — a typed value in the grammar, for instance — closing the pasted-token gap in the cleartext
  refusal too.
