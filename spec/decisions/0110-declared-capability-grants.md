# 0110 — A connector manifest states the host access it needs and the host decides the grant before anything runs

**Status:** accepted 2026-09-18
**Decides:** `connector.declare-capability.refusal.undeclared-access`, `connector.declare-capability.refusal.allowlist-shape`, `connector.declare-capability.refusal.wildcard-beside-attachment`

## Context

A connector is the one way outside data enters the run path. It runs sandboxed with three
imports and no filesystem or socket access, so every reach outward crosses a boundary the
host owns. The question is not whether the host can refuse — it can refuse anything — but
when it decides, and from what.

Two audiences settle a grant. A person reviewing a pull request reads a manifest. An agent
validating a connector before installing it reads the same file against a schema. Neither
can execute the connector across every branch to discover what it reaches for. If the grant
set is a property of which code paths run, it is a property no reviewer can enumerate, and
review degrades into trusting the author.

Timing matters as much as form. A denial issued at the network stack arrives after the
request has been formed: the URL is assembled, the headers are populated, and in the common
shape the credential has already been resolved into the request object. Refusing there
prevents a socket write and nothing earlier. Refusing at load prevents the construction.

The allowlist itself is a matcher over hostnames, and matchers have a characteristic failure:
the entry an author writes expecting "everything" is the entry that, under suffix matching
with a stripped wildcard label, admits nothing. That is a silent inversion — the connector
reaches for a host, is refused, and the author reads the manifest as permissive. The same
matcher is what makes a wildcard beside a bound credential dangerous: material attached to a
declared host is material attached to unboundedly many hosts when the host is a pattern.

## Decision

A connector manifest declares every host access the code reaches for — outbound hosts,
environment names, the clock. The host decides the grant from that file. There is no
run-time permission request and no escalation path. A connector reaching for access its
manifest does not list fails to load with a diagnostic naming that access, raising
`ConnectorUndeclaredAccess`. An empty allowlist, a bare wildcard, an empty entry, and an
entry carrying a scheme, a port or a path each raise `ConnectorAllowlistRejected`. A
connector that binds host-attached material declares one non-wildcard host; a wildcard
standing beside an attachment raises `ConnectorWildcardAttachment`, judged at manifest
validation and again at session open.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Declaration in the manifest, decided at load** *(chosen)* | The grant set is a property of a file two kinds of reviewer can read without running anything. | A connector cannot widen its access from what it discovers; a second host is a file edit and a reload. |
| A run-time `request_permission()` call | Access tracks what the connector actually needs on this input. | Lost on reviewability: the grant set becomes a union over code paths, which no reading of the file enumerates. |
| Ambient access with denial at the network stack | Nothing to declare; the sandbox refuses what it refuses. | Lost on timing: the request is already formed and the credential already resolved when the denial fires, so the control sits behind the point the material moved. |
| Accept a wildcard beside a credential attachment | One entry covers a vendor's whole subdomain space, which is how vendors document themselves. | Lost on attachment scope: the material binds to unboundedly many hosts, and no reader can name the set it reached. |
| Treat a bare wildcard as "all hosts" | The entry means what an author writing it expects. | Lost on consistency: the matcher strips a leading wildcard label everywhere else, so one entry would obey a different rule than its neighbors. |

## Criteria

1. **Reviewability before execution.** Whether the complete grant set is derivable from
   the file alone. *This criterion decided it.* A connector is third-party code the engine
   runs against a consumer's credentials; the only control that scales across authors is
   one a reviewer can apply without trusting the author's account of their own code.
2. **Position of the control relative to the material.** Whether a refusal happens before
   or after the request is formed and the credential resolved.
3. **Nameability of the attachment set.** Whether a reader can name every host a bound
   credential can reach.
4. **Diagnosability of a misspelled entry.** Whether a permissive-looking entry that admits
   nothing produces a diagnostic instead of a puzzle.

## Consequences

Installing a connector becomes a reading exercise with a bounded answer: the hosts, the
environment names, the clock. An agent can gate installation on a schema check. A denial
never has to be trusted to arrive at the right layer, because the layer that would have
issued it is never reached.

The accepted cost: a connector cannot adapt its access to what it discovers. A source that
learns at run time that its vendor redirects to a second domain fails to load, and the
operator edits the manifest and reloads. For vendors whose infrastructure moves — asset
hosts, regional shards, signed-URL domains — that is a recurring edit rather than a
one-time one.

The wildcard-and-attachment refusal is deliberately conservative and blocks a shape that is
sometimes benign: a vendor whose API genuinely spans subdomains under one credential now
needs one entry per subdomain. Whether that becomes onerous is unmeasured: the shape
of real vendor subdomain fan-out across a population of connectors is unobserved.

## Revisit triggers

- Manifests in practice enumerate more than a handful of subdomains for one vendor
  credential, which would make the non-wildcard requirement a maintenance burden rather
  than a bound.
- A connector class emerges whose reachable host set is genuinely data-dependent — a
  crawler, a federated protocol — for which no static list is honest.
- The matcher gains a form that can express "any subdomain, attachment scoped to the
  registrable domain", which would let the wildcard and the attachment coexist with a
  nameable set.
