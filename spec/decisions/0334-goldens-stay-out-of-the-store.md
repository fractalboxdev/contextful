# 0334 — A golden set is never ingested into the store it measures, held by a manifest lint and a connector-side path refusal

**Status:** accepted 2026-09-18
**Decides:** `build.baseline.refusal.answer-key-in-the-corpus`

## Context

A golden set is the answer key: version-controlled canonical cases holding a question, the
expected answer, the artifacts and edges that should be retrieved, and the citations the
answer should carry. The harness measures the read path by comparing a live ranking against
that key.

The key and the corpus live in the same tree. Goldens sit under an evaluation directory,
corpora are ingested from directories, and an ingestion pipeline can name any path. Nothing
about the directory layout stops a pipeline from pointing at the evaluation directory —
by a glob that reaches one level too far, by a root configured at the repository rather
than the corpus, or by a connector fetching third-party content that happens to include a
copy.

If it happens, recall goes up and means nothing. The retriever finds the expected artifact
identifiers because the file naming them is in the index. Precision improves. The judged
dimensions improve, since the reader is reading the answer. Every number moves in the
direction everyone wants, the baseline gate passes, and the improvement is entirely an
artifact of the store holding its own answer key. There is no signal in the report that
distinguishes this from a real gain, and the direction of the error is the direction nobody
investigates.

The two enforcement points differ in what they can see. A manifest lint reads the pipeline
definitions in the tree and can reject an artifact path that normalizes under the
evaluation directory — but it only sees paths the tree declares. A connector-side refusal
of those same normalized paths sits at the point where content is actually read, including
content a third party supplies through a source the manifest never enumerates.

## Decision

A golden set ingested into the store it measures raises `GoldenSetIngested`. Two guards
hold it: a manifest lint rejecting an artifact path that normalizes under the evaluation
directory, and a connector-side refusal of those same normalized paths, which is the
stronger guard over third-party content. The engine's native set lives outside every
ingestion root.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A manifest lint and a connector-side path refusal, with the native set outside every ingestion root** *(chosen)* | Contamination is refused both where a path is declared and where content is read, so a source the manifest never enumerates is still covered. | The engine's native set lives outside every ingestion root, so a deployment wanting to serve those cases as data needs a second, explicitly separate copy. |
| Relying on convention and directory layout | Zero mechanism; the layout already separates the two; contributors know the rule. | Lost on enforcement: an ordinary pipeline can name any path, so the rule is honored by whoever remembers it and violated silently by whoever does not. |
| A manifest lint alone | Cheap, static, catches every path the tree declares, and reports at authoring time. | Lost on coverage over content a third party supplies: the lint reads declared paths, and a connector fetching an external source that contains a copy is outside what it can see. |
| A connector-side refusal alone | Sits at the point of actual reading, so it covers everything that reaches the store. | Lost on timing: it refuses at run time, so a mistake in a manifest is found when a pipeline runs rather than when it is written, and the diagnosis arrives further from the edit. |
| Generating goldens from the store at run time rather than committing them | No file to contaminate with, and truth always matches the corpus. | Lost on the same criterion from the other side: the store then holds its own answer key by construction, and the measurement is circular with no path that is not. |
| Detecting contamination statistically — flagging a suspicious jump in recall | Catches contamination however it arrived, including routes no path check covers. | Lost on precision: a real improvement and a contaminated one produce the same shape of jump, so the check either fires on genuine gains or is tuned until it fires on nothing. |

## Criteria

1. **Whether the measurement means anything** — whether the store can come to contain the
   key it is measured against.
2. **Coverage** — whether a guard sees content a third party supplies, not only paths the
   tree declares.
3. **Timing** — whether a mistake is refused at authoring time or at run time.
4. **Mechanism cost** — guards to build and maintain.

**The first criterion decides it outright.** A store containing its own answer key makes
recall fiction, and every number downstream of it — the baseline comparison, the progression
history, the slice breakdown — inherits the fiction with no marker. Coverage is why there
are two guards rather than one: the lint wins on timing and loses on third-party content,
and the pair costs one more mechanism to cover the case the cheaper one cannot see.

## Consequences

A recall figure is a statement about retrieval. A pipeline that would ingest the evaluation
directory is refused where it is declared and again where it would read, so the mistake is
caught whether it arrives by a glob in the tree or by a source outside it.

The cost accepted: the engine's native set lives outside every ingestion root, so a
deployment that wants to serve those cases as ordinary data — as documentation, as sample
content, as anything a caller can retrieve — maintains a second copy that is explicitly
separate and that drifts from the first. Two guards also means two places to keep the
normalization rules in agreement, and a path that normalizes differently in the two is a
hole neither one reports.

Reversing this is cheap mechanically and destroys the archive: relaxing either guard is one
change, and after any contamination every historical figure becomes unclassifiable, because
nothing in a stored report says which store held what.

## Revisit triggers

- The two guards' path normalizations are found to disagree on a real path, which would
  argue for one shared implementation rather than two.
- A deployment needs the native cases served as data often enough that the separate copy is
  a recurring maintenance cost rather than a one-off.
- A contamination detector appears whose precision is good enough to distinguish a genuine
  gain from a contaminated one, which would cover routes no path check reaches.
