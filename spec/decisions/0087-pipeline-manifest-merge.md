# 0087 — One pipeline id declared twice is a hard error naming both files

**Status:** accepted 2026-09-18
**Decides:** `pipeline.declare.refusal.pipeline-id`, `pipeline.declare.refusal.pipeline-spec`

## Context

Pipelines are discovered rather than enumerated. Startup reads project config and its inline
declaration blocks, then every TOML and JSON file under the pipelines directory, and assembles
one specification set keyed on `id`. Nothing lists the files in advance; the set is whatever the
directory holds.

That discovery makes the id the only name anything else binds to. A schedule fires an id, a
destination table name is folded from an id, a content hash keys a replay against an id, and the
catalog records a run under one. If two files declare the same id and the loader resolves the
conflict rather than refusing it, every one of those bindings resolves against a specification
the author never wrote — and the failure mode is not an error. It is a pipeline that fires its
cadence twice, lands two overlapping runs into one table, and reports success both times.

Directory order is the other constraint. A precedence rule has to order the files, and the only
available order is a filesystem listing, which is neither declared by the author nor stable
across platforms. A rule that says "the later file wins" is a rule whose outcome nobody wrote
down.

## Decision

One `id` declared in two manifest files raises `PipelineDuplicateId`, naming each file with the
line the declaration starts on. The scan refuses before any pipeline is assembled, so no plan
compiles and no source is contacted. Specifications merge on `id` across the discovered files in
the sense that the assembled set is keyed by id; two declarations of one key is an error rather
than a merge.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse, naming both files and both lines** *(chosen)* | A duplicate is a failure at scan with the two sites printed; a double-fire is impossible. | A project that would split one pipeline across two files for review or ownership reasons has no path; the refusal names the collision but offers no composition. |
| Last file wins | Every manifest set loads; an override file can shadow a shared declaration. | Loses on diagnosability: the winner is decided by a directory listing rather than by anything the author wrote, so the effective specification differs between two machines holding identical trees. |
| First file wins | Same as above, with a stable-looking rule. | Lost on whether the effective specification is authored, identically: reversing the order does not make the order authored, so two machines holding one tree still load different sets. |
| Merge field by field across both declarations | A base file plus an overlay expresses environment differences. | Loses on shape: a source block and a table set do not merge into a coherent specification — two sources are two pipelines, and two table arrays have no defined union with respect to keys, ordering and write modes. A partial merge produces a specification whose behavior is stated nowhere. |

## Criteria

1. **Diagnosability of a double-fire** — whether a duplicated declaration can produce a running
   system with nothing erroring. *This is the criterion that decided it.* Every resolving option
   converts an authoring mistake into overlapping runs against one table, which presents
   downstream as duplicated rows rather than as a configuration error, and is discovered by
   noticing a count is wrong. The cost of refusing is borne once at scan by whoever made the
   mistake.
2. **Whether the effective specification is authored** — whether a reader can determine the set
   that will run by reading the files, without knowing the listing order.
3. **One writable home per schedule set** — whether the cadence, the tables and the source for
   one id sit in one place a reader can open.
4. **Composition** — whether large declarations can be split for review. Only the merging option
   scores here, and it scores at the cost of the first criterion.

## Consequences

Assembly is a pure function of the file set rather than of the file order, so two machines
holding the same tree assemble the same pipelines. A rename that accidentally duplicates an id
fails immediately with both paths printed rather than shadowing the original.

The cost accepted is the absence of composition. A team wanting per-environment overlays, or
wanting one large pipeline's tables reviewed in separate files, writes distinct ids and accepts
the duplication, or generates the manifest from something upstream of the scan. The refusal is
deliberately unhelpful about that: it reports the collision and does not suggest a merge path,
since there is none.

## Revisit triggers

- Overlay-shaped demand becomes concrete — the same pipeline needing to differ by environment in
  more than the credentials a secret reference already resolves.
- A manifest generator becomes the normal authoring path, which moves the duplicate check
  upstream and makes the scan's refusal a redundant second gate.
- A declared composition operator appears in the specification itself, at which point merging is
  authored rather than inferred and the diagnosability objection no longer applies.
