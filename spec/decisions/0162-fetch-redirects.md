# 0162 — The host list is re-applied per hop and a scheme downgrade stops the chain

**Status:** accepted 2026-09-18
**Decides:** `derive.fetch.refusal.redirect-downgrade`

## Context

A redirect is a destination chosen by the host at the other end of a request that already passed
every guard. Publishers redirect constantly and for ordinary reasons: a canonical host, a
country edition, a link shortener, a consent interstitial. The address in a row is therefore
rarely the address the document is served from, and the link row's own `link_final_url` column
exists to record where the chain actually landed.

That makes the redirect a hole in the bound 0160 and 0161 establish. A host list checked once, at
the first hop, is a list of hosts permitted to name any other host on the internet as the real
destination. The party doing the naming is the same party whose links are being followed — the
one the list exists to constrain — and the redirect gives them a second, unchecked channel to do
it in. One listed shortener would make the list vacuous.

Scheme is a separate property from host. A chain can stay entirely within listed names and still
step from `https` to `http` on the way, which is a hop a permissive redirect policy follows
without comment. From that hop onward the request line, the final address and the response body
travel in the clear, and the engine that reads the document cannot tell that it happened.

The third question is what a stopped chain looks like. A redirect that is declined is not a
publisher error and not a transport error, and if it surfaces as either the operator reading the
marker learns nothing about what to change.

## Decision

The redirect policy re-applies the host list at every hop and stops rather than follows when a hop
leaves it. A redirect from `https` to `http` raises `DeriveRedirectDowngrade` and the chain stops
there. A chain runs to at most 5 hops. A stopped redirect surfaces as the response, carrying the
name that was declined, and lands in the retryable band so an operator's one-line list edit is not
outlived by a settled marker.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Re-apply the list per hop, stop on a downgrade, surface the stop as the response** *(chosen)* | A listed host cannot hand the client to an unlisted one; no hop travels in the clear; the operator reads the name to add | A publisher whose canonical address legitimately crosses domains needs both names listed |
| Check the list at the first hop alone | Matches what an off-the-shelf client does; no per-hop policy | Loses on the first criterion: a listed host redirects anywhere it likes, so one listed shortener empties the list of meaning |
| Follow a downgrade and record that it happened | Maximum reachability against publishers with broken TLS on a redirect target | Loses on confidentiality: the remainder of the chain travels in the clear, and a recorded fact does not un-send the request |
| Fail silently on a stopped hop, landing a generic transport error | One error path; no new refusal identifier | Loses on legibility: the operator cannot tell which name to add, so the fix is invisible and the marker looks like a dead publisher |
| Follow every hop and check only the final address | Tolerates chains that dip through an unlisted intermediary | Loses on the first criterion in a subtler form — the intermediary saw the request, and the address it was given was chosen by a third party |

## Criteria

1. **Whether a listed host can delegate** — whether the list bounds the destination or only the
   first request.
2. **Confidentiality of the chain** — whether any hop's request line travels where it can be read.
3. **Legibility of a stop** — whether the marker names the change the operator would make.
4. **Reachability** — how much of the legitimate publisher population stays readable.

Criterion 1 decided it. The other three are properties of a bound that already holds; criterion 1
is whether the bound holds at all, and a first-hop-only check converts the host list from a
constraint into a set of parties authorized to choose freely. Criterion 2 then settles the
downgrade independently, on the same logic as 0161's criterion 1: a fact recorded after the bytes
left is not a protection. Criterion 4 is the one that was traded away, and criterion 3 is what
keeps the trade cheap — the operator is handed the name rather than left to infer it.

## Consequences

Easier: `link_final_url` is a trustworthy column, because every host on the path to it was on the
operator's list. The bound the operator wrote is the bound that holds, with no second channel.

Harder: listing a publisher now means listing its canonical redirect targets too — the apex and
the `www` form, the content domain behind a shortener, the regional edition. Each is discovered by
hitting it, which costs one retryable marker and one list edit.

Accepted cost: a publisher whose canonical address legitimately crosses domains needs both names
on the list, and there is no way to know which second name is required until a unit is pointed at
the first. How many publishers this affects is unmeasured and depends entirely on the link
population a given deployment scans.

Expensive to reverse: the hop bound and the per-hop check together mean chains are short and
fully-named. Relaxing to a final-address check later would make historical `link_final_url` values
and current ones carry different guarantees, with nothing on the row to distinguish them.

## Revisit triggers

- Stopped-redirect markers dominate a deployment's failures, observed as the retryable band filling
  with declined hops against names that are subsequently listed.
- A publisher population is encountered whose canonical chains exceed the hop bound, observed as
  chains truncated at the ceiling rather than by the list.
- A downgrade is observed on a path where the final address is `https` again, which would make the
  confidentiality criterion argue about one hop rather than the remainder.
