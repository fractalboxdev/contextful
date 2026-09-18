# 0001 — A normative sentence is one clause row carrying a four-coordinate address

**Status:** accepted 2026-09-18
**Decides:** `corpus.address.shape.clause-id`, `corpus.address.invariant.contract-segment`, `corpus.address.invariant.operation-segment`, `corpus.address.shape.kind-segment`, `corpus.address.invariant.subject-segment`, `corpus.address.invariant.ids-are-stable`, `corpus.address.refusal.duplicate-id`, `corpus.address.refusal.retired-id`, `corpus.anatomy.shape.file-headings`, `corpus.anatomy.invariant.operation-is-bound`, `corpus.anatomy.refusal.unowned-operation`, `corpus.anatomy.limit.file-length`

## Context

This corpus is written before the code and the code is built against it. Twenty contracts
partition it across some twenty files, and a fact stated in one file is a fact some other
file's author will want to lean on: the store's on-disk layout is what a replica reads,
the delegation profile is what a verification accepts, the enforcement order is what a
semantic component inherits. The natural way to lean on a fact is to restate it locally in
one's own words, which is how two files come to describe the same behavior and then drift.

A corpus this size cannot be kept consistent by reading it. Restatement is invisible at
the scale of a single review — the two sentences sit in different files, months apart, and
each one reads correct. What makes drift catchable is a machine test, and a machine test
needs the fact to have an identity that survives rewording.

The system's shape also moves. Files split when they get long, operations move between
files of one contract, and a term's owning contract is settled late. Any addressing scheme
whose keys encode file position or layer depth re-keys itself on every such move, and a
re-key invalidates every reference, every pin, every roadmap row and every record
citation at once.

## Decision

A normative sentence is one row in a clause table, keyed by a four-segment address:
contract, operation, kind, subject. Three of the four segments resolve against the
registry — the contract in `spec/terms/contract.toml`, the operation in
`spec/terms/operation.toml` with exactly one file claiming it under `owns`, the subject in
`spec/terms/term.toml` under this clause's contract — so an author writing a clause about
a fact another contract owns fails a registry join rather than passing a reading. The kind
segment is read off the sentence's own form and is one of `invariant`, `refusal`, `limit`,
`shape`, `interface`, `workflow`. An id changes when a fact changes obligor and at no
other time, so moving an operation between two files of one contract rewrites nothing. A
repeated id and a retired id are both refused at extraction.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Four-coordinate clause rows, three segments registry-resolved** *(chosen)* | Duplication and foreign assertion are joins a script performs, not judgements a reviewer makes. A new sentence's home is computed from its subject and operation. | Every sentence costs an address and usually a registry entry; a refusal costs a record as well. A mechanism spanning contracts reads as rows the reader reassembles by following ids. |
| Prose sections with a citation convention | Reads naturally, near-zero authoring overhead, no registry to maintain. | Lost on detection: a citation records acknowledgement, and the author restates the fact in their own words in the sentence beside it. Nothing distinguishes a pointer from a second copy. |
| Ownership by layer, computed as the lowest layer naming a term | No manual registration; ownership falls out of usage. | Lost on stability: one passing mention of a term in a lower file re-keys the property and every reference to it. Ids move for reasons unrelated to the facts they name. |
| One file per noun, cross-references pointer-only | Ownership is obvious from the filename; no `owns` map to keep honest. | Lost on the same detection criterion: nothing joins a usage back to its owner, so an author needing the fact writes a fresh clause under their own contract rather than a pointer. |

## Criteria

1. **Detection** — whether a fact stated twice is caught by a script rather than by a
   reviewer's memory. **This criterion decided.** The corpus is too large and too
   long-lived for consistency held by reading; three of the four coordinates resolving
   against the registry is what turns a restatement into a failed join, and turns the
   question "does this belong to me?" into a lookup with one answer.
2. **Placement** — whether a reviewer learns where a new sentence belongs without asking.
   The operation segment names exactly one file, so placement is derived.
3. **Stability under reorganization** — whether a file getting long, or an operation
   moving, forces a rename. Ids carry no file position, so neither does.
4. **Authoring cost** — how much a small clarification costs. The chosen option is the
   most expensive of the four here, and lost this criterion to the first.

## Consequences

Duplication, foreign assertion and dangling references become mechanical checks with named
errors and line numbers. A file can split at 900 lines with no id churn, and the length
bound is enforceable exactly because addressing does not rest on file position. Pins,
roadmap rows and record citations all key on the same ids, so one stable identifier ties
behavior to its demonstration and to its argument.

The cost accepted: a sentence is not cheap. It costs an address, usually a registry entry,
and for a refusal a decision record, so a small clarification is not a small change, and an
author with a five-word correction pays a multi-file price for it. A mechanism spanning
several contracts has no single place that narrates it — the reader assembles it from rows
by following ids, and that assembly is work no single prose section does.

Reversing this is expensive in proportion to the corpus: every id is referenced from pins,
the roadmap, the records and the generated tree, so unwinding the addressing means
rewriting all four at once.

## Revisit triggers

- The registry join produces more false refusals than true ones over a quarter of
  authoring — the foreign-assertion arms refuse sentences that are correct as written.
- A single contract's clause count grows past the point where the four coordinates no
  longer distinguish subjects, forcing subject spellings that carry position.
- Authoring cost measurably stalls correction: a known-wrong clause stays wrong because the
  address-plus-registry-plus-record price exceeds the value of the fix.
