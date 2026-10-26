---
contract: corpus
---

# Corpus law

## The problem a corpus solves

Code is built and tested against this specification. A reader therefore needs to find
each obligation, know why it holds and see whether code meets it. Corpus law is the
grammar that lets a program answer those three questions. `contextful-spec lint` checks
every rule, and a rule the checker cannot enforce is not written down.

## One clause, followed through

The example adds an error case to the store's compaction step, the operation named
`fold`. An error case is a condition under which an operation returns a named error.

A contract, such as `store`, owns one or more numbered files. Each file holds one `## `
section per operation, and a one-paragraph lede opens the section to say what the
operation covers ({{corpus.anatomy.lede}}). The author writes one list item under
`## fold`: a backticked subject, `partial-snapshot`, then a statement of at most 40
words ({{corpus.anatomy.statement-words}}).

The item's address joins contract, operation and subject into
`store.fold.partial-snapshot` ({{corpus.address.clause-id}}). Tests, proofs and other
clauses use that address, so splitting the file renames nothing.

The statement names the error, so the author registers the error in the contract's
fragment, `spec/terms/store.toml`, against this one clause
({{corpus.registry.fragment}}). A fragment is the table of a contract's operations,
errors and bounds. A second clause that needs the same condition writes the clause id
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
date. The date fails lint, because no authored file says when or whether something is
built ({{corpus.render.dated-prose}}).

## Build state is computed

`contextful-spec state` lists the new clause as `committed` and adds the error to the
generated store card ({{corpus.render.card}}). A test tagged with the clause id and a
digest of the statement supplies a pin ({{corpus.state.tag-pin}}). The verdict follows
the pins: an ignored test, an unfinished body or a stale tag computes `broken`
({{corpus.state.verdict}}). A reworded statement leaves the tag stale until someone
checks the test against the new wording ({{corpus.state.stale-pin}}).

## Diagrams, targets and guides

A diagram is a fenced Mermaid block with one shape per kind of thing. Stores are
cylinders, and a question is a decision with a labelled exit per outcome
({{corpus.diagram.shape}}). A deployment target file under `spec/targets/` names the
provider primitive filling each role, and the checker refuses a shape that leaves a role
unfilled ({{corpus.targets.incomplete}}).

Each contract has three views. The contract file is reference, and the card is
generated from the fragment. The guide teaches the flow and reaches each rule only by
pointer, so the guide never becomes a second home for a rule
({{corpus.guide.non-normative}}).

The spec cites no literature. Sources live under `references/`, which points into the
spec by operation ({{corpus.reference.no-literature}}).

To hand one operation to an implementer, `contextful-spec slice store.fold` packs the
operation's lede and clauses, the clauses they point at, the cited records, and the
errors and bounds they own.
