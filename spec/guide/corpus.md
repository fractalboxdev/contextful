---
contract: corpus
---

# Corpus law

## The problem a corpus solves

Contributors build and test code against this specification, so a reader needs to find each
obligation, know why it holds and see whether code meets it. Corpus law is the grammar
that lets `contextful-spec lint` answer those questions; the corpus states no rule
the checker leaves unenforced.

## One clause, followed through

The example adds an error case, a condition returning a named error, to the store's
compaction operation `fold`.

A contract, such as `store`, owns one or more numbered files. Each file holds one `## `
section per operation, and a one-paragraph lede opens the section to say what the
operation covers ({{corpus.anatomy.lede}}). The author writes one list item under
`## fold`: a backticked subject, `partial-snapshot`, then a statement of at most 40
words ({{corpus.anatomy.statement-words}}).

The item's address joins contract, operation and subject into
`store.fold.partial-snapshot` ({{corpus.address.clause-id}}). Tests, proofs and other
clauses use that address, and splitting the file renames nothing.

The statement names the error, so the author registers the error in the contract's registry
fragment, `spec/terms/store.toml`, against this one clause
({{corpus.registry.fragment}}). A second clause that needs the same condition writes the clause id
between double braces instead of restating it ({{corpus.reference.pointer}}).

An indented italic line under the item carries its Why: a `because` of at most 30 words
({{corpus.rationale.why-cell}}), or a record id. Records under `spec/adr/` hold the
principles `P1` to `P8` and one decision file per contract, such as `A-store`, with the
options each decision rejected.

Nobody labels the kind of a clause. The statement names a registered error, so the
checker computes the kind `refusal`. A clause owning a bound is a limit, and every other
clause is behavior ({{corpus.anatomy.kind-is-computed}}).

## When the clause breaks a rule

Suppose a second clause also names the error. The checker raises a registry finding,
because one error belongs to exactly one clause
({{corpus.registry.one-error-one-clause}}). Suppose the statement carries a calendar
date. The date fails lint, because no authored file states a date or a
build state ({{corpus.render.dated-prose}}).

## Computed build state

`contextful-spec state` lists the new clause as `committed` and adds the error to the
generated store card ({{corpus.render.card}}). A test tagged with the clause id and a
digest of the statement supplies a pin ({{corpus.state.tag-pin}}, {{corpus.state.pin}}).

An ignored test, an unfinished body or a stale tag computes `broken`
({{corpus.state.verdict}}). A reworded statement leaves the tag stale until someone
checks the test against the new wording ({{corpus.state.stale-pin}}).

## Diagrams, targets and guides

A diagram is a fenced Mermaid block with one shape per kind of thing. Stores are
cylinders, and a question is a decision with a labelled exit per outcome
({{corpus.diagram.shape}}). A deployment target file under `spec/targets/` names the
provider primitive filling each role, and the checker refuses a shape that leaves a role
unfilled ({{corpus.targets.incomplete}}).

Each contract has three views. The contract file is reference, and the checker
generates the card from the fragment. The guide teaches the flow ({{corpus.guide.file}}) and
reaches each rule only by pointer, so the contract file stays the only place stating a rule
({{corpus.guide.non-normative}}).

A guide reaches every operation of its contract ({{corpus.guide.reach}}). Each paragraph
carries prose between its pointers, so a guide stays more than an index
({{corpus.guide.pointer-density}}). A number equal to a bound value travels with that
bound's pointer ({{corpus.guide.bound}}).

A question that spans contracts gets a topic page under `spec/topics/`: at most 600 words
that point at the clauses answering it ({{corpus.topic.file}}).

The spec cites no literature; `references/` points into the spec instead
({{corpus.reference.no-literature}}).

`contextful-spec slice store.fold` packs one operation for an implementer: its lede and
clauses, the clauses they reach, the cited records, and the errors and bounds they own.
