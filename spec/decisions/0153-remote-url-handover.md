# 0153 — An engine dereferences a media address itself when it declares the capability and the operator opted in

**Status:** accepted 2026-09-18
**Decides:** `derive.bind.refusal.remote-url-unsupported`, `derive.bind.refusal.media-unreadable`

## Context

A media value on a parent row is an address far more often than it is a local file. Feeds
publish enclosures as `https` addresses; the bytes live on a publisher's host or a hosting
provider's edge.

Those addresses are not neutral strings. A podcast enclosure routinely carries a subscriber
token in its query string — that is how paid feeds identify the listener — and it can also
carry a per-download tracking identifier. Handing such an address to a transcription vendor so
the vendor can fetch it places the subscriber's token in a third party's request log, where
nothing in this system governs its retention. The saving is real: an engine that accepts
addresses streams the media itself, and the machine never downloads an hour of audio it only
needs to forward.

Engines differ on whether they can take an address at all. Some vendor APIs accept a URL and
fetch it; others accept only an upload; a local binary takes a path on disk and nothing else.
The adapter is the only party that knows which, and it already probes its capabilities once
per run.

There is a second, plainer failure in the same position: a media value that is neither an
address nor a file this machine can read. A path to a volume that is not mounted, a filename
with no directory, a value a connector wrote from a field that was not a path at all.

## Decision

A `Url` reaches an engine when the adapter declares `accepts_remote_url` and the operator
opted in. Both are required; either one alone leaves the media fetched or transcoded locally
by a preprocess step before the engine sees it.

A unit whose media is an `http` or `https` address, handed to an engine that declines remote
addresses, raises `DeriveRemoteUrlUnsupported`, naming the engine and the
`when = "media_is_url"` preprocess step that resolves it. A media value that is neither an
address nor a readable file on this machine raises `DeriveMediaUnreadable` and fails that unit
alone.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Handover on declared capability plus operator opt-in** *(chosen)* | Bandwidth saved where the engine can stream and the operator accepts the exposure; the capability gap is a build-time-known property read once per run | A deployment wanting the handover states it twice — once in the adapter's capability and once in the operator's opt-in |
| Always handing the address over | Simplest, cheapest, no local download ever | Lost on token exposure: an enclosure routinely carries a subscriber token in its query string, and the handover places it in a third party's request log with no retention governed here |
| Always fetching locally | One rule; no address ever leaves the machine | Lost on cost: it downloads and re-uploads bytes an engine that accepts addresses would have streamed itself, doubling transfer for every unit |
| Stripping the query string before handover | Removes the token, keeps the saving | Lost on correctness: the token is frequently what authorizes the fetch, so a stripped address returns a refusal instead of media |
| Capability alone, with no operator opt-in | One declaration; the party that knows decides | Lost on who bears the exposure: the adapter knows what it can do and not whether this deployment accepts a third party seeing its subscriber tokens |
| Opt-in alone, with no capability probe | One declaration, operator in control | Lost on when the gap surfaces: an opted-in deployment against an upload-only engine discovers the mismatch per unit rather than from a probe read once per run |

## Criteria

1. **Whether a token inside a media address reaches a third party's request log** — the
   exposure the query string carries.
2. **Whether a capability gap is discovered per unit** — whether an engine that cannot take an
   address says so once, from its probe, or N times from failed calls.
3. **Bytes transferred per unit** — whether the machine downloads media only to forward it.
4. **Number of declarations a deployment writes** — configuration surface.

Criteria 1 and 2 are both necessary and neither alone is sufficient, which is exactly why the
decision needs two declarations. The exposure question is the operator's to answer and the
capability question is the adapter's; collapsing to one declaration answers one of them by
assumption. Transfer cost lost to both: it is money and time, and the other two are a token
in someone else's log and a per-unit failure table.

## Consequences

A deployment that wants the handover says so twice, and a mismatch between the two halves
produces a refusal naming the preprocess step that resolves it rather than a silent local
fetch. The doubled declaration is the cost accepted.

An unreadable media value fails one unit and leaves the run going, so a mounted-volume mistake
costs the rows it actually affects rather than the whole tick.

What gets easier: swapping an engine changes the fetching behavior automatically, because the
condition that fires the local fetch is probed off the engine's capability rather than written
into the chain.

What is now expensive to reverse: the opt-in is a statement about where subscriber tokens are
permitted to travel, and turning it on retroactively cannot un-log the addresses a vendor has
already seen.

## Revisit triggers

- Enclosure addresses in common use stop carrying credential material, for instance if feeds
  move to per-request authorization headers.
- An engine class arrives that fetches through a mediated client this machine controls, so the
  address never leaves the deployment while the bytes still stream.
