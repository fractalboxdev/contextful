# 0129 — A search provider's host is pinned at the call site rather than read from configuration

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.provider-origin`

## Context

The search source turns a query into rows: one row per result carrying the URL, title,
snippet, publication instant, score, provider, query and fetch instant. It reaches a
commercial search API, and every such API bills per query and authenticates with an
operator-held provider key.

The conventional shape for this is a base URL in configuration and a key beside it. That
shape means one configuration edit — a line in a pipeline manifest, an environment value, a
deployment overlay — redirects every query, with the operator's key attached, to a host of
the editor's choosing. The key is the asset: it is billable, it is rarely scoped to a single
purpose, and its theft is invisible until an invoice arrives. Configuration is also the part
of a deployment with the widest write access; the people and processes that can edit a
manifest are a much larger set than those who can land a code change.

Nothing about a base URL is a legitimate deployment variable here. A commercial search
provider has one API host. An operator never needs to point the provider at a different one,
so the configurability buys nothing an operator wants and costs the only defense the key
has against a configuration edit.

Two cases are genuinely not the provider's production host. A recorded fixture server, which
is how the source is tested without spending billed queries, answers on loopback. And a
non-TLS transport to anything at all carries the key in cleartext.

The search source is also the one source whose failures are routinely transient and
tolerable: an absent provider key and a provider 5xx yield zero rows and a zero-row run
outcome, and a reader answers from the corpus already landed. That tolerance makes it
important that a wrong host is not among the tolerable failures — a misdirected query
carrying the key must fail loudly rather than degrade quietly.

## Decision

Each search provider's host is pinned at the call site rather than read from configuration.
A non-TLS transport, and any host other than that provider's, raise
`ConnectorProviderOriginRejected`. Loopback is excepted so a fixture is pointable. Adding a
provider is a match arm plus an allowlist entry rather than a new pipeline shape.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Host pinned at the call site, loopback excepted** *(chosen)* | No configuration edit can redirect a query carrying the operator's provider key; the set of reachable hosts is readable from the source. | Adding a provider is a code change — a match arm plus an allowlist entry — rather than a configuration edit, so a new provider waits for a release. |
| Taking the provider base URL from configuration | A new provider, a regional endpoint or a proxy is a one-line manifest edit. | Lost on key containment: one edit walks the key to another host, and configuration is the widest-write surface in a deployment. |
| Refusing loopback as well | One uniform rule, no exception to reason about. | Lost on testability: a recorded fixture server is how the source is exercised without spending billed queries, and pointing at it is the point. |
| An operator-maintained host allowlist in configuration | Keeps the pin while letting an operator add a provider without a release. | Lost on the same criterion as plain configuration, one step removed: whoever edits the allowlist chooses the destination, so the defense is only as strong as the weakest edit path. |
| Dropping the key on an unpinned host and continuing | A misconfigured host cannot exfiltrate the key. | Lost on distinguishability: the unauthenticated query returns an error or empty results, which this source is built to tolerate as zero rows — so the misconfiguration reads as a quiet day. |

## Criteria

1. **Whether a configuration edit can redirect a query carrying the operator's provider
   key.** This is the property the decision exists for.
2. **Testability without billed queries.**
3. **How a wrong host surfaces.** Loudly, or as the zero-row degradation this source treats
   as normal.
4. **Cost of adding a provider.**

Criterion 1 decides. Criterion 4 is the cost and it is real — a release cycle stands between
an operator and a new provider — but the comparison is between a scheduling inconvenience
that someone notices and a key exfiltration that nobody does. Criterion 3 rules out the
drop-the-key variant, which would otherwise be a reasonable middle.

## Consequences

The complete set of hosts a search query can reach is readable from the source, which makes
it reviewable in the same place as the code that builds the request. A fixture server on
loopback exercises the whole path — request shape, paging, row mapping — with no billed
query and no provider key. A misdirected host fails with a named refusal rather than
degrading into the zero-row outcome this source treats as ordinary.

The cost accepted: adding a provider is a code change and therefore a release. An operator
who wants a provider the engine does not carry is blocked until one ships, and a provider
that changes its API host forces a release rather than a manifest edit. For a deployment on
a slow release cadence that delay is measured in weeks.

The loopback exception is a real hole with a narrow shape: anything the deployment can reach
on loopback can receive queries and keys. It is accepted because a process able to listen on
the engine's own loopback has already won.

## Revisit triggers

- A provider in use moves or regionalizes its API host, making the pin the reason queries
  fail.
- Operators need providers faster than the release cadence supplies them, which would argue
  for a signed provider list rather than for plain configuration.
- The provider key gains per-destination binding at the credential layer, at which point a
  redirected query cannot carry usable material and the pin is no longer what protects it.
