# 0122 — A vendor-supplied link is followed within the configured origin, with one upgrade hop exempt

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.transport-downgrade`

## Context

Two of the four pagination shapes hand the source a URL the vendor wrote: a next-URL path
read out of the response body, and a `Link` header. The source then issues that URL with the
manifest's `[attach]` headers on it — which is to say, with the operator's credential. The
URL is a field of a response body. A response body is attacker-influenced input on any API
that reflects user-controlled text, and it is wholly attacker-controlled the moment the
endpoint itself is compromised, misconfigured, or reached through a resolver that has
drifted.

The manifest already names the hosts the connector may reach. That allowlist is judged at
the host mediation point ahead of socket I/O, so a next link to an unrelated host is refused
there. It is not sufficient on its own: the allowlist is often written with a wildcard for a
vendor's CDN, it does not constrain the scheme, and it does not constrain the port. A link
from `https://api.vendor.example/v1/x` to `http://api.vendor.example/v1/x` stays inside the
allowlist and takes the bearer token to the same host over cleartext.

Checking the link alone is also not sufficient, because a redirect moves the request after
the check. A vendor URL that passes the origin test can answer `302` to a third party, and
the credential travels on the redirected request unless the client is told otherwise. The
landed URL — the one the response actually came from — is the fact worth judging.

One hop is a genuine false positive if it is refused. A vendor that publishes a cleartext
endpoint and answers every request with a redirect to TLS at the same host and port is
forcing an upgrade, not moving the request. That hop cannot reach another party: it changes
the scheme and nothing else.

## Decision

A vendor-supplied next link is followed within the configured origin — scheme, host and
port — and the origin is judged a second time against the URL the response actually landed
on. A hop from TLS to cleartext at the same host, and any hop leaving the configured origin,
raise `ConnectorTransportDowngrade`; the redirect status surfaces as the response and the
read fails. One hop is exempt: a cleartext endpoint landing on TLS at the host and port as
written.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Origin pinning on the link and on the landed URL, TLS upgrade exempt** *(chosen)* | A response body cannot walk the operator's credential off the origin the manifest named, whether by link or by redirect. | A vendor that legitimately moves its apex to a `www` host or to a CDN refuses where it worked before, and the check runs on a live response, so nothing surfaces the affected pipeline ahead of the run. |
| Following any link the vendor returns, bounded only by the host allowlist | Zero false refusals; every vendor's paging works as documented. | Lost on credential containment: a wildcard allowlist entry, a drifted DNS answer or a compromised endpoint carries an authorization header to a host the manifest never individually named, over a scheme it never named. |
| Checking the link and not the landed URL | Cheaper — one comparison, no redirect inspection, and redirects follow transparently in the HTTP client. | Lost on credential containment for the same reason in a different position: the check passes and the redirect moves the request afterwards. |
| Making off-origin expansion configurable | Covers the apex-to-CDN vendor with a manifest key instead of a code change. | Rejected on the same criterion: an escape hatch whose only safe implementation drops the credential on the hop, which then fails at the vendor and reads as a broken pipeline anyway. |
| Dropping the credential on an off-origin hop and continuing | The read continues; nothing is exfiltrated. | Lost on distinguishability: the off-origin request returns 401 or an empty page, and a truncated walk reads as a finished one. A refusal names the cause. |

## Criteria

1. **Credential containment.** Whether any input under a vendor's control can cause the
   operator's credential to reach a party the manifest did not name.
2. **Whether the check survives a redirect.** Whether the judged URL is the one the request
   ended at.
3. **False-refusal rate against real vendor topology.** Apex-to-`www`, apex-to-CDN, and
   cleartext-to-TLS are all common.
4. **When the failure is discoverable.** Build, validation, or first live run.

Criterion 1 decides. Criterion 3 is where this decision pays its cost and where the TLS
upgrade exemption comes from — but a false refusal is a failed read with a named cause and a
one-line manifest fix, while a containment failure is a credential in someone else's logs
that nothing in the system reports. The asymmetry is not close, so the strictest option that
still admits the forced-upgrade hop wins.

## Consequences

Every credentialed walk is bounded to one origin, and the bound holds under redirect. An
operator reading a `ConnectorTransportDowngrade` gets the configured origin and the landed
URL, which is enough to tell a vendor migration from an attack without a packet capture.

The cost accepted: a vendor migration that is entirely benign — apex to `www`, or paging
links served from a CDN host — refuses, and refuses on a live run rather than at build or at
validation, because the landed URL exists only once a response has arrived. The size of this
is unmeasured: how often the vendors in use move paging links off the configured origin is
not something the system observes today, so the false-refusal rate is unknown rather than
known-small. The fix is an operator editing the configured origin, which is a manifest
change and a redeploy.

Judging the landed URL requires the HTTP client to expose per-hop URLs rather than following
redirects opaquely, which constrains the client the host mediation point is built on.

## Revisit triggers

- A vendor in use serves paging links from a host other than its configured origin and the
  manifest edit does not cover it — for example links alternating across CDN hosts per page.
- The host allowlist gains per-scheme and per-port granularity, at which point part of this
  check is expressible as a capability declaration rather than as source behavior.
- A `ConnectorTransportDowngrade` rate high enough that operators are widening origins to
  silence it, which converts the refusal into the escape hatch this decision rejected.
