# 0253 — A container allowlist is default-deny and evaluated all-of

**Status:** accepted 2026-09-18
**Decides:** `visibility.pack.refusal.any-of-allowlist`

## Context

Some sources expose which containers a row belongs to — a channel, a space, a folder —
and expose nothing about who may read the container. The mirror has no grant rows to
join against for such a table, so the whole audience question collapses onto one
manifest list of container identifiers the operator is willing to serve.

A row's membership is a set, not a value. One message lives in one channel; one document
lives in a folder that a second folder also links; a thread carries both the open
engineering channel and the private incident channel. The evaluation rule therefore has
to say what happens when a row's set is partly listed, and that is the entire decision.

The asymmetry is that the two candidate rules fail in opposite directions and only one
failure is recoverable. Under all-of, a row whose set includes an identifier nobody
listed is dropped, and the operator sees a gap they can close by extending the list.
Under any-of, a row filed under one open container and one restricted container is
served to everyone who reaches the open one, and nothing in the deployment reports that
the restricted filing was overridden.

An absent or empty membership set is the same question asked with no evidence. Such a
row carries nothing at all about its intended audience, and admitting it is admitting a
row on the strength of a missing field.

## Decision

A container allowlist is default-deny. A row survives when every container in its
membership set appears in the list; an unrecognized identifier drops the row, and an
empty or absent membership set drops it. An allowlist evaluated as any-of raises
`VisibilityAllowlistAnyOf` at load.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **All-of over the whole membership set, default-deny** *(chosen)* | A row reaches a reader only where the operator listed every container that filed it, so a restricted filing is never overridden by an open one. | Material the deployment is entitled to stays dark until the list is completed, and the gap is visible only as an absence in answers. |
| Any-of — one listed container admits the row | Fewer surprising absences; a row filed anywhere open is servable immediately. | Loses on which rows are wrongly admitted: a row also filed under a container nobody listed is served on the strength of the open filing alone. |
| Admit rows carrying an empty or absent membership set | Sources with patchy container metadata land usefully without operator work. | Loses on the same criterion. An unfiled row carries no evidence of its audience, so admitting it admits on missing data. |
| Deny-list the restricted containers instead | Matches how operators describe the problem — name the private places. | Loses on default: a container created after the list was written is served, and containers are created constantly. |

## Criteria

1. **Which rows the rule wrongly admits.** *This criterion decides.* The contract's
   whole claim is that a reader sees inside the audience the source already drew. A rule
   that admits a row against one of its own filings breaks that claim silently, while a
   rule that drops a row breaks it loudly and recoverably.
2. **Whether the failure is observable to the operator.** A dropped row shows up as a
   missing answer someone reports; a wrongly served row shows up as an incident.
3. **Behavior on data the source did not send.** A rule keyed on an absent field decides
   on nothing.
4. **Completeness of the served corpus.** Given up where it conflicts with the first.

## Consequences

Landing a container-roster source is front-loaded operator work: the list has to be
enumerated before the table answers anything, and each new container is a manifest diff.
That diff is reviewable, which is the point, but it is also a standing chore proportional
to how fast the source's containers multiply.

The accepted cost is the mixed-filing row. A thread filed under one listed and one
unrecognized container is dropped, and its content is absent from answers a reader was
entitled to receive. Nothing distinguishes that absence from the material simply not
existing, so the reader experiences an incomplete answer rather than a refusal.

Reversing to any-of is cheap in code and expensive in fact: every dropped row
becomes servable at once, across every reader, with no per-row record of what changed.

## Revisit triggers

- A source begins emitting per-row audience data directly, which moves the table off the
  allowlist path entirely and onto the mirrored join.
- Operators are observed extending the list reactively after reported gaps rather than
  ahead of ingestion, which means the front-loaded work is not actually happening.
- A source appears whose container sets are large and mostly generated, where all-of
  drops nearly everything and the list can never be completed by hand.
