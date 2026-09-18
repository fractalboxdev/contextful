# 0050 — Without the embedded SQL engine, both read tools refuse outright

**Status:** accepted 2026-09-18
**Decides:** `read.embed.refusal.absent-read-backend`

## Context

The read face has two tools a caller reaches: a statement tool that runs caller-written SQL
against registered relations, and a ranked tool that generates candidates and orders them.
Both execute inside one embedded SQL engine. The lexical index and the approximate
nearest-neighbour index are arms that feed candidates into that engine; they are not
substitutes for it.

A build can omit the engine. Size-constrained deployments have a real reason to want the
binary without it — the run path, the connector families and the commit path are all
reachable without any read tool at all, and the engine is the single largest linked
component.

An absent arm and an absent engine are different in kind. The contract already makes the
arm absences distinguishable at the call: the ranking function changes, or the approximate
arm contributes no candidates while a supplied embedding still scores over what the recency
window recalled. In both cases the caller receives a correct answer over a narrower
candidate set. There is no narrower path that covers arbitrary SQL, and no narrower path
that covers a ranked read once the engine executing the ranking is gone.

A response carries no marker saying which half of the read path produced it.

## Decision

Without the embedded SQL engine linked, both read tools refuse outright with
`ReadBackendAbsent`. Neither answers a ranked read from the lexical arm alone, and neither
returns an empty result. The packaging profile therefore decides whether a binary has a read
capability at all, and a binary without one says so on the first call.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Both tools refuse** *(chosen)* | A caller never receives a narrower answer that reads as a complete one; the absence is visible on the first call. | A deployment that trims the engine for size has no read face, so binary size and read capability are one choice rather than two. |
| Answer ranked reads from the lexical arm alone | The ranked tool keeps working in a small binary, covering the most common read shape. | Loses on distinguishability: the response carries no marker that the structured half was absent, so a caller cannot tell a narrowed answer from a complete one. |
| Return an empty result | No error path to handle; existing clients keep running. | Loses on the same criterion, and worse: it presents absence as emptiness, which every caller reads as "the store has nothing". |
| Refuse the statement tool, degrade the ranked tool | Splits the difference — the tool that genuinely needs arbitrary SQL refuses, the other serves. | Loses on distinguishability for the ranked tool, which is the tool whose answers are hardest to audit, and adds a second rule for one capability. |

## Criteria

1. **Distinguishability** — whether a caller holding only the response can tell a degraded
   answer from a complete one. *This criterion decided it.* Every rejected option produces
   a response that is indistinguishable from a correct one, and the read face's whole value
   is that a caller can act on what it returns. A wrong answer that announces itself costs
   a retry; a wrong answer that does not costs a decision made on it.
2. **Deployment flexibility** — whether a build profile exists that trims the engine and
   keeps something useful. The chosen option scores worst here, and this is the cost
   accepted.
3. **Rule count** — how many distinct behaviors describe an absent component. One rule for
   the engine and one for each arm beats a per-tool matrix, but this only broke the tie
   between the two rejected degradation options.

## Consequences

The packaging profile decides the read capability. A deployment choosing a small binary
chooses a binary that answers no read, and that trade is made at build time where it is
visible, rather than at call time where it is not.

Callers get one error to handle rather than a class of silently-narrowed results. The
handshake's reported set names the engine, so a client that cares declares a requirement
and is refused earlier still.

The cost of the absent engine is now paid entirely by the deployments that chose it, and
those deployments have no partial read path to fall back on — not a lexical-only search,
not a table listing. Whether any deployment actually wants a read-less binary is
unmeasured; the profile exists because the run path does not need the engine, not because a
caller asked for it.

## Revisit triggers

- A response envelope gains a field naming which arms contributed, at which point a
  narrowed answer becomes distinguishable and degradation is back on the table.
- A deployment profile ships in volume without the engine and its operators ask for a
  lexical-only search rather than no read face.
- The engine stops dominating binary size, removing the reason the trimmed profile exists.
