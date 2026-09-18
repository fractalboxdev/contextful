# 0160 — A link engine reaches an operator-enumerated host list with no wildcard

**Status:** accepted 2026-09-18
**Decides:** `derive.fetch.refusal.empty-host-list`, `derive.fetch.refusal.wildcard-host`, `derive.fetch.refusal.malformed-host-entry`

## Context

A `fetch` engine's whole task is following an address a third party wrote. The unit's media
column holds a link that arrived from a feed, a message or a page — content the machine ingested
precisely because it did not author it. Every other network destination in the system is named in
configuration; this one is named by the publisher of the row. The engine is therefore a request
generator whose destination is chosen by whoever can get a row into a scanned table.

The binding split makes the natural home for a host list the wrong one. A pipeline manifest can
arrive from anywhere — it is the request side, carrying a name and its bounds and nothing
executable — and it is authored by the same party whose links are followed. A manifest that
declared its own reachable hosts would be a third party granting itself permission.

Host lists also fail in a particular direction. An operator writing an allowlist writes it once,
under pressure, and the entries are strings. An empty list reads as "not configured yet" to a
person and as "reaches nothing" or "reaches everything" to an implementation, depending on how the
membership test was written. The matcher this engine uses strips a leading `*.` before comparing
and then tests equality, which means an entry written as a bare `*` is not a wildcard at all: it
falls through to the equality branch and admits only a host literally spelled that way. An
operator who writes it has expressed "everything" and received "nothing", with no error to read.

## Decision

A fetch engine reaches hosts named in `[derive.<name>].allow_hosts` and no others. The list is
required and the party that authors it is the party that owns the machine. An `allow_hosts` list
with no entries raises `DeriveHostListEmpty`. A bare `*` entry raises `DeriveHostListWildcard`.
An empty entry, or one carrying a scheme, a port or a path, raises `DeriveHostEntryMalformed`
naming the entry. Subdomain coverage is written as a leading `*.` against a suffix; the apex name
is listed on its own.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A required operator-authored host list, no bare wildcard, malformed entries refused** *(chosen)* | The destination is bounded by the machine's owner; a mistake is a refusal rather than a silent posture | The operator maintains a list against a link population they do not control |
| Pin to a configured origin, as a plain HTTP source does | One name, nothing to maintain, strongest possible bound | Loses on applicability: the task is following a link a publisher wrote, so the manifest cannot name the host in advance and the engine would have no work |
| Declare hosts in the pipeline manifest | Colocated with the pipeline that needs them; portable | Loses on the binding split: the manifest is authored by the party whose links are followed, so the grant and the grantee are the same party |
| Allow a bare `*` as "any host" | An operator who trusts the link population can say so in one character | Loses on behavior: the matcher strips a leading `*.` and falls to equality, so a bare star admits a host literally spelled that way — an entry meaning everything admits nothing, which is worse than an error |
| Treat an empty list as "reach nothing" without refusing | No configuration required to be safe | Lost on failure direction of an operator mistake: it fails closed but silently, so a binding nobody finished looks identical to one deliberately closed and the operator learns from an empty output table |

## Criteria

1. **Third-party choice of destination** — whether a party outside the machine can determine where
   a request goes.
2. **Failure direction of an operator mistake** — whether a wrong or unfinished list fails closed
   and loudly, or open and quietly.
3. **Applicability** — whether the rule leaves the task doable at all.
4. **Legibility of the list** — whether an entry's effect is readable from the entry.

Criteria 1 and 2 decide together. Criterion 1 alone would pick the configured-origin option and
delete the feature; criterion 3 rules that out, so the bound has to be a list, and once it is a
list criterion 2 governs every remaining choice — which is why an empty list, a bare star and a
malformed entry are three refusals rather than three tolerated spellings. Criterion 4 is why the
star refusal names what the matcher actually does rather than simply rejecting the character.

## Consequences

Easier: reasoning about a fetch engine's reach is reading one key. A stopped request names the
host it wanted, so extending the list is a one-line configuration change made with the evidence in
hand.

Harder: a link population that legitimately spans many publishers — a general reading list, a
shared inbox — needs a list that grows with it, and every unlisted host costs a settled marker
before anyone notices. An operator maintaining that list is doing work proportional to the
diversity of the content, which is the content's property, not the engine's.

Accepted cost, stated with its size: nothing resolves the host and checks the resulting address,
so an operator-listed name whose DNS answers a private address is reached. This is tolerable
because a name reaches the list by a human writing it down, which is a different threat model from
a name arriving in a row — but it is a real residual, and it is unmeasured how often an
operator-listed name is under a third party's DNS control.

Expensive to reverse: the refusal on a bare star teaches operators that wildcards are spelled with
a leading `*.`. Introducing a true any-host spelling later would have to pick a different token,
because the obvious one now means something else.

## Revisit triggers

- Host lists grow past the point where an operator reviews them, observed as entries added in bulk
  in response to markers rather than deliberately.
- A listed name is observed resolving to a private address, which is the accepted residual
  arriving and would force the post-resolution check to be decided.
- A legitimate task requires reaching a publisher population that cannot be enumerated in advance,
  making criterion 3 pull against criterion 1 again.
