# 0141 — Redirect pinning binds every source, credential-bearing or not

**Status:** accepted 2026-09-18
**Decides:** `secret.attach.refusal.weakened-hop`

## Context

A declaration states one host per source, and the landed rows carry whatever the origin at
that address returned. An operator reading a table reasons from the address they declared to
the contents they see, and the read path's guarantees about provenance rest on those two
being the same thing.

A redirect breaks that identity without touching the declaration. The party answering the
configured address names a new URL and the client fetches it. What lands is a body from an
origin nobody declared, in a column that goes out to readers of the store. The obvious place
to guard is the credential: a hop carrying a bearer token to a new host replays material, and
that is the sharpest harm. But the uncredentialed case lands the same foreign body in the
same column, and the read path cannot tell the difference afterwards.

There is a hop worth allowing. A cleartext base that redirects to TLS at the same host and
port is the same endpoint upgrading its transport — the vendor is not naming a new party, it
is refusing to answer on the weaker one. The reverse, TLS down to cleartext, is a
downgrade and is where a credential would be exposed.

The boundary has to be drawn somewhere, and "same registrable domain" is the tempting place,
since a vendor's endpoints usually share one. The set of parties behind one registrable
domain is not something the declaration states and not something the engine can enumerate;
shared-hosting domains and per-customer subdomains both sit inside it.

## Decision

Every source, credential-bearing or not, follows a hop whose next URL keeps the configured
host and port on the same transport or a stronger one. A move from cleartext to TLS at that
same host and port is followed. A hop from a TLS base to a cleartext URL, and a hop naming
any other host or port, raise `SecretRedirectOffOrigin`: the redirect status becomes the
response and the read fails. The origin answering the request whose body lands therefore
equals the configured host and port, so a column's contents and the address an operator
declared cannot come apart.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Pin every source to the configured host and port, allow cleartext-to-TLS at that address** *(chosen)* | The landed origin equals the declared origin for every source, and a transport upgrade is not an outage | A vendor that legitimately moves an endpoint needs a declaration edit rather than being followed |
| Pin only credential-bearing sources | Covers the sharpest harm with the narrowest rule | Lost on landed content: an uncredentialed source is redirected into landing another party's body, in a column a reader trusts, and nothing downstream can detect it |
| Follow same-registrable-domain hops | A vendor's own migrations and regional endpoints work without edits | Lost on sharpness: the set of parties behind one domain is not something the declaration states, so the rule permits hops to parties the operator never named |
| Follow a TLS URL named from a cleartext base, to any host | Cleartext bases upgrade wherever the vendor points | Lost on provenance: there the vendor is naming a new URL rather than upgrading the one configured, and the improvement in transport says nothing about who answers |
| Follow no redirect at all | One rule, nothing to reason about | Lost on a real cost with no gain: refusing the same-address transport upgrade breaks vendors that redirect cleartext to TLS as policy, and that hop changes neither the party nor the exposure |

## Criteria

1. **Whether the landed origin equals the declared origin** — that a column's provenance is
   recoverable from the declaration. *This is the criterion that decided it.* A redirect
   steers a request as much as a credential does, and the body answered at a new address lands
   in a column that goes out to readers, so the harm is not confined to sources that carry
   material.
2. **Credential exposure on a hop** — whether material is replayed at a new party. Real, and
   a subset of the first: everything it forbids the first criterion already forbids.
3. **Precision of the permitted set** — whether the rule's boundary is something the
   declaration states. Decides against the registrable-domain option, which is expressible
   but not derivable from what an operator wrote.
4. **Vendor compatibility** — how many legitimate vendor behaviors the rule breaks. It is
   what the accepted cost is paid in, and it is what keeps the same-address upgrade legal.

## Consequences

An operator reading a landed column knows which origin produced it without consulting a run
record. The guarantee holds uniformly, so a reviewer does not have to determine whether a
source carries a credential before reasoning about where its body came from. A vendor that
starts redirecting to a new host is a refused read with the status in hand, rather than
silently changed data.

The accepted cost: a vendor that legitimately moves an endpoint needs a declaration edit
rather than being followed. Until that edit lands the source fails, which for a scheduled
pull means a gap in the table rather than a wrong value — the intended direction, and still an
outage the vendor did not cause. Multi-region vendors that steer by redirect are unusable
without one source per region.

`Referer` is off on every outbound client for the same reason, keeping a previous URL and its
query string off a followed hop.

## Revisit triggers

- A declaration grammar gains an explicit allowed-origin set, so a vendor's own alternate
  origins are stated rather than inferred.
- Refused hops are observed dominated by same-vendor canonicalization — trailing slashes,
  case, path normalization — at the configured host, indicating the port or path handling is
  stricter than the criterion requires.
- A read-path mechanism records the answering origin per landed row, at which point pinning and
  recording become alternatives rather than the former being the only way to know.
