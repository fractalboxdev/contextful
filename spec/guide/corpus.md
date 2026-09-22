---
contract: corpus
---

# Corpus law

## What it is for

The specification is a contract that code is built and tested against, so every
obligation in it has an address, one home, a reason, and a place in computed build
state. Corpus law is the grammar that makes that true, and `contextful-spec lint`
checks all of it; a rule the checker cannot enforce is not written down.

## How to read a contract file

A contract, such as `store` or `authority`, owns one or more numbered files. Each file
opens with a paragraph and usually a diagram, then holds one `## ` section per
operation: a verb such as `fold`, `lease` or `attenuate`. The first paragraph under the
heading is the operation's lede and says what it covers
({{corpus.anatomy.lede}}). A list of clauses follows.

Each list item is one obligation, in the present tense, as a standing fact. The
backticked word that opens the item is its subject, and the item's full address is the
contract, the operation and the subject joined by dots, so `partial-snapshot` under
`## fold` in the store file is `store.fold.partial-snapshot`
({{corpus.address.clause-id}}). That address is what tests, proofs and other clauses
use.

An indented italic line under an item is its Why. It is either a short `because`
stating the deciding criterion, or the id of a record under `spec/adr/`: `P1` to `P8`
are principles every contract obeys, and `A-store`, `A-run` and so on hold one
contract's decisions with the options each rejected
({{corpus.rationale.why-cell}}, {{corpus.rationale.contract-adr}}). Read the record when
you want to know what else was on the table.

Nobody labels a clause's kind; the checker computes it. A clause that names an error
is a refusal, one that owns a named number is a limit, and everything else is behavior
({{corpus.anatomy.kind-is-computed}}). Each error and each number belongs to exactly
one clause; anywhere else in the corpus reaches it by writing `{{`, the clause id and
`}}` ({{corpus.registry.one-error-one-clause}}, {{corpus.reference.pointer}}).

After the list, a section may carry prose, diagrams, `#### Scenarios` (worked WHEN/THEN
examples attached to a clause) and `unsettled:` lines naming an open question, its
owner and the operation it affects ({{corpus.anatomy.scenario}},
{{corpus.rationale.unsettled-line}}). None of that states an obligation.

## What the corpus never says

No authored file says whether something is built, and none carries a date. Build state
is computed: a clause pinned to a test or a Lean theorem is `performed` when that
artifact exists and runs, and `broken` when it does not, is ignored, or was written
against older wording ({{corpus.state.status-is-computed}},
{{corpus.state.verdict}}, {{corpus.state.stale-pin}}). Words that date a
sentence are refused outright ({{corpus.render.dated-prose}}), and so are the modals
that describe a path not taken ({{corpus.render.counterfactual}}).

The spec never cites literature. Sources live under `references/`, which points into
the spec by operation name ({{corpus.reference.no-literature}}).

## Three views of one contract

- **The contract file** is reference: every rule, one per line, for implementing and
  testing against.
- **The guide** under `spec/guide/` teaches the flow with a worked example and reaches
  every rule by pointer, so it is never a second home for one
  ({{corpus.guide.non-normative}}).
- **The card** under `spec/cards/` is generated: each operation with its lede, each
  refusal with its trigger, each bound with its value and basis
  ({{corpus.render.card}}).

## Worked example

To add a refusal to the store's fold: write one item under `## fold`, naming the error
in the statement; register the error in `spec/terms/store.toml` against that clause
({{corpus.registry.fragment}}); give the item a Why. Run the checker. It computes the
kind as refusal and writes the clause into the lock file. The next `state` run lists it
as `committed` and adds the error to the store card. When a test tagged with the
clause id and its statement digest lands, status flips to `performed`
({{corpus.state.tag-pin}}). Reword the statement later and the tag goes stale until
the test is checked against the new wording.

To hand the work to someone, run `contextful-spec slice store.fold`: it packs the
operation's lede and clauses, every clause they point at, the records they cite, and
the errors and bounds they own.
