# 0113 — A connector with a declared limiter reaches its vendor with a granted reservation or does not reach it at all

**Status:** accepted 2026-09-18
**Decides:** `connector.meter.refusal.quota-unbound`, `connector.meter.refusal.binding-transport`, `connector.meter.refusal.unreadable-answer`, `connector.meter.refusal.unmetered-request`

## Context

A vendor quota is shared. The consumer's own application already spends it — checkout calls,
support lookups, whatever moves money — and a batch read is a burst of requests against the
same ceiling. An ingest that paces itself perfectly by its own reckoning still starves the
traffic that matters, because its own reckoning cannot see anyone else's. The quantity that
needs coordinating lives outside the engine, and a coordinator outside the engine is the
only thing that can hold it.

The host already sits at the point where this is enforceable. Every guest request crosses
the outgoing-HTTP import, and every compiled-in source that reaches a vendor goes through
the engine's shared client. That is also the unit the vendor itself meters: one outbound
request. A reservation taken at the call level is unenforceable, one call issuing any number
of requests for paging, retries and token refresh.

Three degradations are available to get wrong. A declared quota with no binding is a
connector that believes it is metered and is not. A limiter answer the engine cannot parse
is an answer, and treating an unparsed answer as permission means a limiter serving an error
page grants unlimited permits. An unreachable limiter is the common operational case — a
restart, a network partition — and it is precisely the moment when failing open sends an
unpaced burst at a vendor nobody is watching.

The limiter binding is itself a credential over the wire. Its endpoint is called on every
permit batch, and its bearer travels with each call.

## Decision

A connector carrying a declared limiter makes no outbound vendor request without a granted
reservation. A declared quota with no binding raises `ConnectorQuotaUnbound` at load. A
limiter endpoint that is not HTTPS, loopback excepted, and a token written as an inline
literal rather than a reference, each raise `ConnectorLimiterBindingRejected` at load. A
limiter response the engine cannot read raises `ConnectorLimiterUnreadable` and is not
treated as permission. An unreachable limiter or an absent binding raises
`ConnectorUnmetered` rather than proceeding. Reservation happens at the mediation point,
one permit per outbound request, granted in batches under a short TTL and reconciled
granted-against-spent in the following report.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Reserve at the mediation point, fail closed on every degradation** *(chosen)* | The engine's vendor traffic is bounded by a quantity that sees all traffic, including traffic the engine does not make. | A second network round trip per permit batch, and permits held by a crashed run are reclaimed only on TTL expiry. |
| A local token bucket inside the engine | No external dependency; no round trip; no new service to run. | Lost on what it can see: it caps the engine's own rate with no view of the consumer's other traffic, which is the traffic the quota exists to protect. |
| Fail open when the limiter is unreachable | Availability: an ingest survives a limiter restart. | Lost on the failure it permits: an unmetered burst is exactly the draw the capability removes, and it arrives during an outage, when nobody is reading dashboards. |
| Reserve per connector call rather than per request | One reservation per unit of work; far fewer round trips. | Lost on enforceability: one call issues any number of requests for paging, retries and refresh, so the permit count bounds nothing the vendor counts. |
| Accept an unparsed limiter answer as a grant | Robust to limiter version drift. | Lost on failure direction: an error page becomes an unlimited grant, which is the strongest possible failure in the wrong direction. |

## Criteria

1. **Whether an engine read can starve the consumer's money-moving traffic.** *This
   criterion decided it.* The engine is a background process on somebody else's production
   quota. Every other criterion here trades cost against convenience; this one is the reason
   the capability exists, and no design that leaves the failure reachable satisfies it.
2. **Whether the reservation unit matches the unit the vendor meters.** Enforceability of
   the permit count.
3. **Direction of each degraded case.** Whether unknown resolves toward refusal or toward
   permission.
4. **Custody of the limiter's own credential.** Whether it can sit in a committed file or
   travel in the clear.
5. **Round-trip and latency cost.** How much the coordination adds per batch.

## Consequences

An operator who binds a quota gets a real ceiling across every party that respects it, and
a report reconciling granted against spent that makes over-grant visible rather than
inferred. A connector author declares a quota name and a traffic class and writes no pacing
code. The limiter counts requests and does not price them, so a monetary ceiling remains
arithmetic performed outside the engine.

The accepted cost is availability coupled to a second service. When the limiter is down,
ingest stops — not degrades, stops — and that is a deliberate exchange of freshness for
protection of the consumer's foreground traffic. The round trip per permit batch adds
latency proportional to batch size, and a run that crashes holding permits leaves them
unspendable until the TTL expires, which over-restricts the quota for that window. The TTL
value that balances reclaim latency against grant churn is not established from live
traffic.

Replay makes no limiter call, since a replayed read issues no outbound request. A denied
reservation returns to the guest as a synthesized 429 that never leaves the host, so a
connector already mapping vendor throttles needs no further interface and cannot tell which
party throttled it.

## Revisit triggers

- Permit-batch round trips become a measurable share of a read's wall-clock time, which
  would argue for a longer TTL or a locally cached grant window.
- A deployment runs with no limiter at all for a meaningful class of connectors, which
  would mean the declaration is being omitted to avoid the failure mode rather than because
  no quota is shared.
- Crashed-run permit reclaim on TTL expiry is observed to over-restrict a quota enough to
  affect the foreground traffic the design protects.
