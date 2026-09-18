# 0121 — A walk declares one pagination shape and stops on a token it has already seen

**Status:** accepted 2026-09-18
**Decides:** `connector.source.refusal.pagination-ambiguity`, `connector.source.refusal.page-loop`

## Context

The generic HTTP source reaches vendors that page four different ways: a page number with a
start index, a cursor read out of the response body and echoed into a query parameter, a
full next URL in the body, and a `Link` header. A pipeline manifest declares which of the
four applies. Nothing about the four shapes is mutually exclusive as configuration — a
manifest can carry a `page_param` and a `next_cursor_path` at once, because they are
separate keys in separate positions of the same table.

A manifest carrying two shapes is almost always assembled by copying one vendor's block over
another's and editing part of it. The two shapes then disagree about what the next request
is, and the disagreement is invisible: both produce a well-formed request, both get a 200,
and rows land. A page-parameter walk against a cursor vendor re-reads page 1 forever or
walks a parallel ordering the cursor was tracking; a cursor walk against a page-parameter
vendor terminates on the first response with no cursor field and lands one page as if the
stream were that short.

The second failure is on the vendor's side. A vendor that hands back the cursor it was just
given, or a next link equal to the URL just fetched, is a live production shape — it happens
on an exhausted result set, on a partially-degraded shard, and on an API version that echoes
request parameters into its own paging block. The walk has a hard ceiling of 1000 requests,
so the loop terminates, but it terminates after spending a thousand requests of the
operator's quota and the vendor's rate budget on one page fetched a thousand times, and the
run reports whatever the dedup path leaves behind.

## Decision

A walk declares exactly one of the four pagination shapes. A manifest declaring two raises
`ConnectorPaginationAmbiguous` at build, ahead of any request. During the walk the source
remembers every cursor token and every next URL the vendor has served; a repeat raises
`ConnectorPageLoop` and fails the read rather than landing that page a second time. The
1000-request cap stays as the ceiling for a vendor that emits fresh tokens forever.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One declared shape, repeat token refuses** *(chosen)* | Two declared shapes are a build error with a named file and key; a vendor loop is one failed read with an error naming the repeated token. | A vendor that genuinely alternates paging shapes across its own endpoints needs an authored connector rather than the generic source. |
| Preferring one shape by a fixed precedence | No build error, no manifest edit, and a walk always starts. | Lost on ambiguity: a manifest carrying both is a copy from two vendors, not a stated preference, so precedence picks the wrong one as often as the right one and pages silently. |
| Re-landing a repeated page and relying on the dedup path | Tolerates a vendor that echoes a token once and recovers on the next request. | Lost on quota: dedup corrects the rows, not the spending. An unbounded echo burns the full 1000-request cap and the vendor's rate budget before the run ends. |
| Warning on two shapes and continuing | The pipeline stays runnable while an operator fixes the manifest. | Lost on distinguishability: a warning in a log beside a successful run reads as a healthy pipeline, and the wrong pages have already landed. |

## Criteria

1. **Ambiguity is not silently resolved.** Whether honoring one of two declared shapes
   produces an observable failure or a run that reports success over the wrong pages.
2. **Spend is bounded by the failure, not by the cap.** Whether a vendor loop costs one
   request or the walk's full ceiling.
3. **Coverage of real vendor paging.** Whether the four shapes reach the vendors a generic
   HTTP source is pointed at without an authored connector.
4. **Manifest edit distance.** How much an operator rewrites when a vendor changes paging.

Criterion 1 decides. Criteria 2 and 4 both have workable answers under every option — the
cap bounds the loop eventually and the manifest edit is small either way. Criterion 1 is the
one where the options diverge on whether the failure is observable at all: precedence and
warning-and-continue both end in a completed run whose row set is wrong and whose position
has advanced, and a position committed over the wrong pages is not recoverable by re-running.

## Consequences

A manifest assembled from two vendors' documentation fails at build with both offending keys
named, which is where it is cheapest to fix. A vendor echoing a token fails one read with the
token in the error, and the committed position from the previous read is where the next run
resumes — no rows are lost and none are duplicated.

The cost accepted: a vendor whose paging genuinely alternates between two shapes — a cursor
on the first page and a page number afterwards, or a `Link` header on some endpoints and a
body cursor on others — is unreachable through the generic source and needs an authored
connector, which is a compiled artifact, a manifest, a pin and a conformance suite rather
than four lines of TOML. That vendor shape exists and is not rare among older APIs.

Loop detection holds every served token for the length of one walk. At the 1000-request cap
with cursors of ordinary length this is bounded well under a megabyte, but the bound is a
consequence of the request cap rather than a stated one.

## Revisit triggers

- A vendor in use requires two paging shapes on one stream, and the authored-connector cost
  is paid more than once for the same reason.
- The page cap is raised, making the held token set an unbounded memory cost rather than one
  bounded by the request ceiling.
- A vendor is observed to legitimately re-serve a cursor — a resumable cursor that is stable
  across pages by design — making `ConnectorPageLoop` a false refusal.
