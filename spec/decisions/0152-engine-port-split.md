# 0152 — The shared port holds what every engine owes the run record and nothing more

**Status:** accepted 2026-09-18
**Decides:** `derive.bind.refusal.endpoint-host-is-bare`, `derive.bind.refusal.confidence-out-of-range`

## Context

Two derive modalities exist and more are expected. A transcription engine answers with timed
intervals over a recording. A link engine answers with a document's head facts and the picture
candidates it advertises. A future document engine would answer with a page number and a
polygon on it.

The shape of the answers has no overlap. A timed interval in seconds, a document head, and a
page plus a two-dimensional polygon share no operation that sorts, merges, dedupes or renders
one citation across all three — and polygon coordinate units do not even agree between vendors
that produce them. Any union type over the three is an anchor whose only usable operation is a
match on which variant it holds, which pushes the branch into every caller instead of removing
it.

What every engine does share is an obligation to the run record. Whatever it produced, a
reader of a derived row needs to know which engine produced it, what host it talked to if any,
where the inference physically happened, and what the engine has to say about its own run. All
four are the same shape regardless of modality.

Two of those shared values arrive from vendor code and land in columns many readers touch.
`endpoint_host` is one: vendors hand back full endpoint addresses, and an address's path
carries tenant identifiers while its query string carries tokens. `confidence` is the other:
a raw model score is meaningful only against the model that produced it, and two vendors'
scores are not on one scale, so a number carried through unchanged reads as a comparable
measurement when it is not.

## Decision

Every engine implements `Deriver`, which owes four things and no more: `engine_id`,
`endpoint_host`, `locality` and `run_audit`. Modality-specific operations live on anchor
traits above it — `Transcriber` adds `capabilities()` and `transcribe(media, opts)`;
`LinkReader` adds `capabilities()`, `read_link(target)` and `unmetered_egress()` — and the
source holds a union over those anchors rather than one trait object.

`endpoint_host` is a host and nothing more; a value carrying a path, a query string, a port or
a scheme raises `DeriveEndpointHostNotBare`. `confidence` is normalized to `0.0..=1.0` at the
adapter rather than passed through as a raw model score; a value outside that interval raises
`DeriveConfidenceOutOfRange` and the field lands null rather than clamped to a plausible
number.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A narrow shared port plus one anchor trait per modality** *(chosen)* | The shared part is exactly the run-record obligations; each modality keeps a typed answer with operations that mean something | Each new modality is a new trait plus a new arm in the engine union, and the source holds the union rather than one trait object |
| A single polymorphic enrich returning a union anchor | One trait, one call site, no union in the source | Lost on the union anchor having no common operation: an interval, a document head and a page polygon share nothing that sorts, merges, dedupes or renders one citation, so every caller matches on the variant anyway |
| A structurally typed answer — a map of fields per modality | Extensible with no code change per modality | Lost on the same criterion plus checking: nothing constrains what a vendor adapter writes, and the mismatch surfaces as a missing key at landing rather than at build |
| Passing a raw model score through as `confidence` | Preserves whatever signal the vendor has | Lost on comparability: two vendors' scores are not on one scale, so the column stops meaning one thing across rows |
| Clamping an out-of-range confidence into the interval | No nulls, no refusal, every row carries a number | Lost on truth: a clamped value reads as a measurement, and 1.0 written because a vendor sent 4.2 is a fabricated certainty |
| Storing the full endpoint address a vendor reports | Maximum provenance detail | Lost on exposure: the path holds tenant identifiers and the query holds tokens, and this value lands in a column many readers reach |

## Criteria

1. **Whether a union anchor has any common operation** — whether one polymorphic answer type
   supports work that is not a match on its variant. **This criterion decided the port split,
   alone.** A shared abstraction that no caller can use without unwrapping it is not an
   abstraction; it relocates the branch and adds a type.
2. **Comparability of a value across vendors** — whether a number in one column means the same
   thing on two rows.
3. **What a value exposes when it lands in a widely-read column** — whether credential or
   tenant material can travel into it.
4. **Cost of adding a modality** — how many declarations a new answer kind touches.

## Consequences

Adding a modality is a new anchor trait and a new arm in the engine union the source holds,
and every place that matches the union gains a branch. That is the cost accepted: the
extension point is typed rather than open.

Rows lose data in two places deliberately. An engine that reports an endpoint address loses
everything but the host. A vendor score outside the normalized interval lands null, so a
consumer filtering on confidence sees fewer usable rows rather than plausible wrong ones.

What is now expensive to reverse: `confidence` is a normalized column in committed tables, so
switching to raw vendor scores later would leave two incomparable populations under one name.

## Revisit triggers

- Two modalities arrive whose answers do share a sortable, mergeable citation anchor, making a
  common operation available.
- A vendor-score calibration lands that makes cross-vendor scores comparable, so normalization
  at the adapter stops being the only way to get one scale.
