---
contract: corpus
owns:
  - address
  - anatomy
  - registry
  - reference
  - rationale
  - state
  - render
  - diagram
  - targets
  - guide
  - topic
---

# Corpus law

This corpus is the specification of **Contextful**, written before the code and built
against. This file is the grammar every other file obeys, and it obeys that grammar
itself. `contextful-spec lint` implements every clause below; a local run and the gate
invoke the identical command, and a rule the checker cannot enforce is not a rule.

```
spec/
  00-corpus.md          this file
  01-topology.md        the system, its contracts, its build profiles
  NN-*.md               one or more files per contract, listed in terms/contract.toml
  terms/                contract.toml, unit.toml, refused-names.toml,
                        and one <contract>.toml fragment per contract
  adr/                  P<n>-<slug>.md principles, A-<contract>.md decisions
  guide/                <contract>.md, one teaching page per contract
  topics/               <slug>.md, one page per cross-contract question
  glossary.md           generated
  cards/                <contract>.md, generated
  pins.toml             clause id -> demonstrating artifact, plus the coverage floor
  roadmap.md            milestone -> operation set
  status.md             generated
  spec.lock.json        generated
references/             literature and practice; points into spec/, never the reverse
tools/spec/             the checker
formal/                 Lean 4 models and theorems, pinned like tests
```

## address

The three segments of a clause id and what makes each legal.

- `clause-id` — A clause id is three dot-separated segments — contract, operation, subject — each matching `[a-z0-9-]+`. A clause item writes the subject alone; its file and section supply the rest.
  *A-corpus#clause-addressed-sentence*
- `contract-segment` — The first segment is the file's front-matter `contract`, which names an entry of `spec/terms/contract.toml` listing that file.
  *P8*
- `operation-segment` — The second segment is the `## ` section holding the item: an operation registered in the contract's fragment and listed in the front-matter `owns` of exactly one of the contract's files.
  *P8*
- `subject-segment` — The third segment is a slug of at most 40 chars, unique within its operation. It is a name, not a registry entry.
  *P8*
- `ids-are-stable` — An id changes when a fact changes obligor. Moving an operation between two files of one contract rewrites no id.
  *P8*
- `malformed-id` — A clause id violating the shape, or a clause item under a section its file does not own, raises `SpecMalformedId`.
  *P8*
- `duplicate-id` — A clause id appearing in two items raises `SpecDuplicateId`, naming both locations.
  *P8*

## anatomy

The section order of a contract file, the lede, clause list and groups of each operation, and the computed kind of a clause.

- `file-headings` — A contract file carries front matter with `contract` and `owns`, one `# ` title matching its contract entry, one `## <operation>` section per owned operation in `owns` order, then an optional `## Shapes`.
  *P8*
- `lede` — An operation section opens with a lede: one paragraph stating what the operation covers. The lede is the operation's only description; its fragment entry carries no gloss.
  *A-corpus#clause-addressed-sentence*
- `clause-list` — After the lede, one contiguous list holds the section's ungrouped clauses, each item `` - `<subject>` — <statement> `` on one line, optionally followed by its Why on a two-space-indented `*…*` line.
  *A-corpus#clause-addressed-sentence*
- `group` — After an operation's clause list, each `### <group>` section holds a one-sentence lede and its own contiguous clause list. A group names no segment of a clause id and follows no content after a list.
  *because a long operation reads as named parts, and a group renames no clause*
- `group-clauses` — A clause list, an operation's or a group's, holds at most 30 clauses; each list past it counts toward the `anatomy-long-lists` ceiling.
  *because a reader scanning one list for its rule loses the thread past about thirty items*
- `word-ceilings` — The `contract-words` ceilings count each contract's statement, Why and lede words, and the `adr-words` ceilings each contract ADR's words outside fences, table rules and section slugs; a count off its ceiling raises `SpecWordCeiling`.
  *because the number of clauses, not their length, drives the corpus's size, and a recorded total makes each addition visible in review*
- `long-operation` — `contextful-spec state` prints an advisory naming each operation holding more than 30 clauses; the advisory refuses nothing.
  *because a long operation is a grouping candidate, and the grouping is a reader's judgment no checker makes*
- `after-list` — Prose, diagrams, tables, scenarios and unsettled lines follow the clause list and state no obligation.
  *A-corpus#clause-addressed-sentence*
- `scenario` — A `#### Scenarios` list under an operation holds items `` - `<clause id>`: WHEN <trigger>, THEN <outcome> `` or a backticked `tests/fixtures/` path; the lock file attaches each to its clause.
  *because an example is the test an implementer writes first, and a rule without one leaves the agent to invent it*
- `bad-scenario` — A scenario naming a clause outside its operation, or holding neither a WHEN/THEN sentence nor a fixture path, raises `SpecScenario`.
  *P8*
- `kind-is-computed` — A clause is a refusal when its statement names an error its fragment assigns to it, a limit when its fragment assigns it a named bound, and a behavior otherwise. The checker writes the kind into `spec/spec.lock.json`.
  *P8*
- `statement-words` — A clause statement holds at most 40 words, a `{{id}}` reference counting as one.
  *because one item states one obligation; a longer item is two clauses or carries rationale*
- `file-length` — A contract file holds at most 900 lines.
  *because a file past that splits by adding a path to its contract entry, which renames nothing*
- `bad-anatomy` — A missing, extra or out-of-order `##` section, a missing lede, a broken list, a malformed group or item, a title off the registry, an oversized file or statement, or a long-list count off its ceiling raises `SpecAnatomy`.
  *P8*

An operation section, with one clause carrying its Why, and an unsettled line:

```markdown
## fold

Compaction: pass order, triggers, retention, and the pointer commit that makes a snapshot readable.

- `partial-snapshot` — A reader observes a snapshot and every declared sidecar together or neither; a commit exposing one without the other raises `StorePartialSnapshot`.
  *P4*

unsettled: Does a replica that has never pulled report an empty store or refuse the read? owner: read-path affects: read.serve
```

## registry

Where a spelling is registered, the single owner of an error or a bound, the collision test, the source sites checked against each error, the ratchets the registry records, and the term table with the glossary and first-use rule it drives.

- `fragment` — Each contract owns `spec/terms/<contract>.toml`, holding its operations, and its errors and named bounds each with a gloss.
  *P8*
- `one-error-one-clause` — An error entry names the one clause raising it. That clause's statement names the error, and no other clause statement does; a second site reaches the condition by `{{id}}`.
  *P2*
- `bound-entry` — A named-bound entry carries its owning clause, a value, a unit from `spec/terms/unit.toml` and a basis: `chosen`, `measured:<benchmark>` or `standard:<name>`. The owning statement carries the value followed by the unit.
  *because an unmeasured number presented as a measurement is a hypothesis stated as fact*
- `unregistered` — An unregistered error after `raises`, an entry naming a missing clause, an error absent from its clause or present in another, and a malformed bound raise `SpecRegistry`.
  *P8*
- `defining-site` — A Rust variant whose `#[error]` text opens `<Id>:`, or a string literal holding `<Id>` alone or opening `<Id>:`, under `crates/*/src`, `tools/*/src`, `apps/*/src` or `formal/`, defines the registered error `<Id>`.
  *because the checker compares code against the registry only where the code spells the identifier*
- `variant-shape` — An error has at most one defining variant. It is named the error, or the error less the prefix its enum's name holds before `Error`, and its enum's other variants define errors of the same contract.
  *because a second variant, a foreign name or a mixed enum hides from a reader of the code which contract raises the error*
- `variant-gloss` — A defining variant's `///` text before its clause pointer is absent or equals its error's registry gloss byte for byte; text after the pointer documents the variant.
  *because the fragment is the one home of a gloss, and a variant copy that differs states a second meaning*
- `ceiling` — The `[ceiling]` table of `spec/terms/corpus.toml` records each ratchet's live count. A count above its ceiling raises the ratchet's error, and so does a count below it until `contextful-spec ceilings` lowers the ceiling.
  *because a ratchet that only refuses growth leaves a reduction unrecorded, and the next change spends it unseen*
- `drift` — A breach of {{corpus.registry.variant-shape}} or {{corpus.registry.variant-gloss}}, a performed refusal whose error has no defining site, and committed refusals with one past the `registry-committed-sites` ceiling raise `SpecRegistryDrift`.
  *because a committed clause with code is work status cannot see, and a performed refusal with no code passes on its test alone*
- `term` — A fragment's `[term]` table maps a lowercase term to the clause of its own contract that defines it, a gloss of at most 25 words, and optional `avoid` spellings the term replaces.
  *because a reader assumes two words name two things, and one registered spelling per meaning makes that assumption hold*
- `bad-term` — A term registered in two fragments, a term naming a missing clause or another contract's clause, an empty or longer gloss, and an `avoid` spelling that is itself a term raise `SpecTerm`.
  *because one spelling naming two meanings is the homonym the term table exists to refuse*
- `glossary` — `contextful-spec state` renders `spec/glossary.md`: every registered term in alphabetical order with its gloss and its defining clause as a pointer; a committed copy differing from regeneration counts under {{corpus.render.generated}}.
  *because a definition a reader cannot find by name serves only its writer*
- `vale-terms` — `contextful-spec state` writes `.vale/styles/WriteTechnically/Terms.yml`, a Vale substitution rule mapping each `avoid` spelling to its term; a committed copy differing from regeneration counts under {{corpus.render.generated}}.
  *because the registry decides each term, and a hand-edited copy of its list drifts from it*
- `term-first-use` — A guide's first use of a registered term, in any case or inflection, sits in a paragraph carrying the term's defining clause as a pointer; first uses without one past the `term-first-use` ceiling raise `SpecTermFirstUse`.
  *because a reader meets a term at its first use, and a definition reached later arrives after the misreading*
- `prose-report` — `contextful-ci prose` runs Vale 3.25.1 with `.vale/vale.ini` over `spec/guide/` and `references/`, prints each alert and their count, and exits 0, naming an absent or other Vale version instead of refusing.
  *because Vale finds candidates a reviewer decides, and a check that cannot judge an alert refuses nothing*

## reference

The one legal way a statement reaches a fact another clause or a code type owns, and the ban on restatement and literature.

- `pointer` — `{{<clause id>}}` is the one way a statement reaches a fact another clause owns. The lock file records every pointer as an edge.
  *P8*
- `dangling` — A `{{id}}` naming no clause raises `SpecDanglingReference`.
  *P8*
- `type-pointer` — `{{type:<crate>::<module>::<Type>}}` names a Rust struct or enum in place of its keys; the lock file records its file, line and each field or variant with its `serde` spelling and doc comment, and the contract's card lists them.
  *because a key list copied from code drifts from the code, and the type already holds every key with its gloss*
- `type-unresolved` — A type pointer whose crate and module part names no file defining that struct or enum raises `SpecTypeUnresolved`.
  *because a pointer to a moved or renamed type shows the reader no keys*
- `restates-code` — `contextful-spec state` prints a `SpecRestatesCode` advisory naming each clause that backticks three or more keys of one deserialized struct or enum and carries no pointer to it; `spec/status.md` records their count.
  *because a key list in a clause is a second home for its type, and which list a pointer replaces is a reviewer's judgment*
- `no-literature` — A spec file carries no hyperlink to an external document and no path under `references/`; either raises `SpecExternalLink`. Literature lives in `references/`, which cites clause ids.
  *because the contract is judged by behavior, and a citation invites arguing from authority*

## rationale

Where an argument lives: the Why line, the record and its anatomy, and the words a contract file cannot carry.

- `why-cell` — A clause's Why is absent, one or more record ids such as `P3`, `A-store` or `A-topology#commit-lock`, or `because` followed by at most 30 words stating the deciding criterion. A refusal carries a Why.
  *P8*
- `record-threshold` — A decision earns a section in its contract's ADR when two or more clauses depend on it, it spans contracts, or it names a revisit trigger; otherwise it lives in a `because` cell.
  *P8*
- `record-anatomy` — A principle `P<n>-<slug>.md` carries `# <id> — <title>`, a `**Status:**` line, then Context (optional), Decision, Options, Consequences, Revisit (optional), in at most 400 words.
  *because a principle argues one rule every contract obeys, and the length of the argument is part of its quality*
- `contract-adr` — A contract's decisions live in one `A-<contract>.md`: `# A-<contract> — <title>`, a `**Status:**` line, then one `## <slug> — <decision>` section per decision, its slug unique in the file, each holding its options table in at most 250 words.
  *because one file per contract puts every decision beside the clauses it governs, and a fresh corpus has no history to keep apart*
- `options-table` — A principle's Options and each ADR section hold one table headed `Option`, `Lost on`, `Cost` with two to five rows. One row is marked *(chosen)* with `—` as Lost on; every other row names the criterion it lost on.
  *P8*
- `unsettled-line` — An unknown is one line where it applies: `unsettled:`, a question ending `?`, `owner:` and a handle, `affects:` and a `<contract>.<operation>`. Another form, or a heading `Open questions`, `Out of scope` or `See also`, raises `SpecUnsettled`.
  *P8*
- `orphan-record` — A record no Why cites, a Why naming no record or section, a record breaking its anatomy, and a section below {{corpus.rationale.record-threshold}} in an ADR every citation of which names a section raise `SpecRecord`.
  *P8*
- `why-scoped` — A Why citing a contract ADR names its section as `A-<contract>#<slug>`. Bare `A-<contract>` citations count toward the `why-unscoped` ceiling; past it, each raises `SpecWhyUnscoped`.
  *because a slug cites the one decision a clause rests on, and a whole-file citation hands the reader every decision of the contract*
- `why-migrate` — `contextful-spec why-migrate <A-id>` rewrites each bare citation of that ADR to the one section naming the clause by id, else by operation, and prints each citation without a single candidate, per contract.
  *because a rewrite the checker cannot justify from the text belongs to a reviewer*
- `cited-by` — `contextful-spec state` lists each contract ADR section with the clauses citing it in its contract's card, and `extract` records the same lists in the lock file.
  *because the lists derive from Why lines, so no hand-written list repeats them*

## state

Pins, verdicts, the coverage floor, the roadmap's operation claims and each milestone's acceptance test.

- `status-is-computed` — `contextful-spec state` computes which clauses the tree demonstrates: per contract and milestone into `spec/status.md`, per clause into the lock file's `pins`. No authored file says whether something is built.
  *P8*
- `pin` — `spec/pins.toml` maps a clause id to one artifact: a `test` function path, a `theorem` constant under `formal/` or an `item` path. A refusal or a limit takes a test or a theorem.
  *P8*
- `verdict` — An unpinned clause computes `committed`; a pinned clause computes `performed` when its Rust, Lean or {{corpus.state.typescript-tag-pin}} artifact qualifies, and `broken` otherwise. A test carrying an `#[ignore]` attribute computes `broken`.
  *P8*
- `tag-pin` — A line `// spec: <id>@<rev>` among the comments and attributes above a test function under `crates/` or `tools/` pins clause `<id>` to that test; `<rev>` is the first 8 hex digits of the statement's SHA-256.
  *because a pin written beside its test travels with every move and rename, and the digest records which wording the test demonstrates*
- `typescript-tag-pin` — A `// spec: <id>@<rev>` line above a uniquely named `node:test` call under a direct TypeScript surface pins its clause when the callback contains a recognizable assertion call.
  *P8*
- `typescript-unfinished` — A pinned TypeScript test using `skip`, `todo` or `only`, lacking an assertion, placing its first assertion after a nested arrow, holding `if (false)` or a placeholder, or shadowing `node:test` computes `broken`.
  *because a test that cannot demonstrate an assertion demonstrates no clause*
- `lean-tag` — A line `-- spec: <id>@<rev>` above a Lean `theorem` or `lemma` under `formal/`, past comments, docstrings and attributes, pins clause `<id>` to that theorem, with `<rev>` as in {{corpus.state.tag-pin}}.
  *because a theorem's pin then moves with the proof, and a reworded clause marks the proof stale*
- `theorem-beside-test` — A clause carries at most one theorem pin beside its test pins. A theorem pin alone states a property of a model, and performs once its proof holds no `sorry`.
  *A-assurance#source-change-carries*
- `bound-theorem` — Beside test pins, a theorem pin counts only when the `binding` of its row in an `inventory.toml` under `formal/` names one of those tests; an uncounted theorem leaves the clause's verdict to its tests.
  *because a proof of a model the code never drives says nothing about the code the clause names*
- `proved` — A performed clause computes `proved` when a counted theorem pin and a test pin of it both perform; `spec/status.md` counts proved clauses per contract and each card per operation.
  *because performed and proved are different strengths of evidence, and one count hides which a contract holds*
- `proof-obligation` — `formal/obligations.toml` lists clause ids under `clauses`; a listed clause that does not compute `proved` raises `SpecProofObligationUnpinned`, so an entry joins once its theorem counts and stays while it does.
  *because a proof dropped silently reads as a clause still proved*
- `stale-pin` — A tag whose `<rev>` differs from the digest of its clause's current statement raises `SpecStalePin`, and its clause computes `broken`.
  *because a reworded clause no longer says what its test was written to demonstrate*
- `several-tests` — A clause takes any number of test pins and computes `performed` only when every one of them performs.
  *because a conjunctive clause takes one test per conjunct, and one passing test of half a clause reads as the whole*
- `unexercised` — A test pin of a refusal clause computes `broken` unless its function body names one of the clause's errors: the id itself, or a `::` variant path keeping at least two of the id's trailing words.
  *because a test that never observes the error demonstrates some other behavior*
- `limit-const` — A named-bound entry may record the Rust const holding its bound as `const = "<file>::<NAME>"`; a test pin of its clause computes `broken` while that const resolves to nothing or no non-test source under `crates/*/src` or `tools/*/src` reads it.
  *because a test of a bound the code never consults demonstrates a number, not a limit*
- `module-path` — A path pin resolves only where its file or module part points: `<file>.rs::<fn>` to that file, `<crate>::<module>::<fn>` to a file of that package opening that module path, `A.B.<theorem>` to `A/B.lean`.
  *because a pin matching any same-named function elsewhere demonstrates nothing about the named one*
- `test-pin-is-test` — A `test` pin naming a function that carries no `#[test]` attribute does not resolve.
  *because a helper or production function proves presence, not behavior*
- `unwired` — `contextful-ci unwired` reports each public item under `crates/*/src` that a performed pin's test names and no non-test source reaches, unless an `// entry: <clause id>` comment names the clause.
  *because a clause performing through its test alone demonstrates code no binary runs*
- `unwired-ceiling` — `tools/ci/unwired.toml` records a `ceiling` for the unwired count; a count above it raises `SpecUnwired`, and `contextful-ci unwired --lower` sets it to a lower live count, so the ceiling only falls.
  *because the count starts above zero, and wiring each item belongs to its own contract's change*
- `unfinished-test` — A pinned test holding a body line that opens with `todo!`, and a pinned Lean declaration holding `sorry` or `admit`, compute `broken`.
  *because a placeholder demonstrates nothing, whether it panics at run time or proves by assumption*
- `coverage-floor` — `spec/pins.toml` carries a per-contract floor of pinned clauses. A live count below its floor raises `SpecCoverageRegression`.
  *because deleting a failing pin must not read as progress*
- `bad-pin` — A pin naming no clause, a tag above no declaration, an item pin on a refusal or limit, a broken or unexercised pin, and one clause pinned to two theorems raise `SpecBrokenPin`.
  *P8*

### commands

The commands that write tags, rewrite revisions and list the clauses a test file or a diff reaches.

- `scaffold` — `contextful-spec scaffold <contract>.<operation> --package <path>` writes one tagged test per refusal and limit clause of the operation into `<path>/tests/integration/`, its body a `todo!` naming the error or bound, and rewrites no existing function.
- `scaffold-lean` — `contextful-spec scaffold <contract>.<operation> --lean <file>` appends one tagged theorem per clause of the operation, its statement the docstring and its proposition and proof `sorry`, and rewrites no existing theorem.
- `migrate` — `contextful-spec pins --migrate` moves each `test` entry of `spec/pins.toml` into a tag above the one test it names, and leaves an entry naming no clause or no single test in place.
  *because a tag travels with its test, and a path in a separate file goes stale unseen*
- `retag` — `contextful-spec retag <clause>` rewrites every tag of the clause to its statement's rev, and a `///` copy above a tag reading at least 0.9 like the tagged statement, refusing while a check other than `state` or `render` reports.
  *because a reworded clause otherwise leaves each tag and copy for a hand edit, and a malformed statement takes no rev*
- `slice-file` — `contextful-spec slice --file <path>` prints each clause the file's tags name, with its current statement and the functions tagging it.
  *because a reviewer of a test reads what it claims to demonstrate*
- `slice-diff` — `contextful-spec slice --diff <rev>` prints each clause whose tag or tagged function holds a line the working tree changes against the merge base of `<rev>` and `HEAD`; the `test-first` stage prints it into the gate log.
  *because an edited test can stop demonstrating its clause while every pin still computes performed*

### milestones

Roadmap names, acceptance tests, milestone verdicts and the clauses no milestone schedules.

- `roadmap` — `spec/roadmap.md` names operations as `<contract>.<operation>` or `<contract>.*`. A name resolving to no operation, an operation claimed by two milestones, or a milestone lacking its `Reach:` or `Acceptance:` line raises `SpecRoadmap`.
  *P8*
- `acceptance` — A milestone's `Acceptance:` line names one test under `crates/acceptance/`; that test computes `absent` when undefined, `open` when ignored or holding a body line that opens with `todo!`, and `passing` otherwise.
  *A-assurance#source-change-carries*
- `closed` — A milestone computes `closed` when its acceptance test computes `passing` and every operation it names holds a `performed` clause, and `open` otherwise.
  *because one passing acceptance test drives one path through a milestone, and an operation without a performed clause demonstrates nothing*
- `acceptance-first` — A milestone holding a pinned clause while its acceptance test computes `absent` raises `SpecAcceptanceMissing`.
  *A-assurance#source-change-carries*
- `unowned` — An unpinned clause outside every operation an open milestone names, and named in backticks by no `unsettled:` line of its operation, is unowned; unowned clauses past the `clause-unowned` ceiling raise `SpecClauseUnowned`.
  *because a clause no test pins, no milestone schedules and no open question names is a promise nobody keeps*
- `deferred-depth` — A milestone carrying a `Depth: operation` line admits only refusal and limit clauses; a behavior clause of an operation it schedules raises `SpecDeferredBehavior`.
  *because an unopened milestone fixes what it refuses and bounds, and its behavior is written against the code that opens it*

## render

The generated files, and the vocabulary no authored file carries.

- `generated` — `contextful-spec state` regenerates `spec/status.md`, `spec/targets.md`, `spec/cards/` and `spec/spec.lock.json`, and `contextful-spec extract` the lock file alone. A committed copy differing from regeneration raises `SpecStaleRender`.
  *P8, A-assurance#generated-artifacts-prove*
- `lock-file` — The lock file carries every clause with its segments, computed kind, statement, Why and location, each operation's lede, the per-file `owns` map, the registry, the pointer graph, and each pinned clause's pins: kind, path, name, line, verdict, proof.
  *P8*
- `card` — `contextful-spec state` renders `spec/cards/<contract>.md` per contract: each operation with its lede and clause count, each refusal as error and gloss, each bound as value, unit and basis, all naming their clause.
  *A-corpus#teaching-reference-live*
- `dated-prose` — The words `planned`, `not yet`, `currently`, `today`, `shipped`, `implemented`, `previously`, `used to`, `legacy`, `TODO`, `FIXME` and `WIP`, and an ISO date, raise `SpecDatedProse` in any authored file.
  *because build state is computed, and a dated sentence is stale the day it lands*
- `counterfactual` — The words `will`, `would` and `shall` raise `SpecCounterfactual` in a contract file or a guide.
  *because a contract states present behavior, and a record's Options is where a path not taken is described*
- `banned-vocabulary` — The nouns `seam`, `load-bearing`, `wedge`, `rung`, `land-grab` and an unbackticked `axiom`, a word whose digest `spec/terms/refused-names.toml` lists, a bare `#<digits>` and a pull-request link raise `SpecBannedWord`.
  *P8*
- `local-path` — A path beginning `/Users/`, `/home/`, `$HOME/` or `~/` raises `SpecLocalPath`.
  *P8*
## diagram

The fenced diagram, and the shape every flowchart, sequence and state diagram keeps so a reader sees the parts and what flows between them.

- `fence` — A diagram is a fenced `mermaid` block; box-drawing characters outside a fence raise `SpecAsciiDiagram`.
  *P8*
- `boundary` — A diagram draws a contract, party, process or trust zone as a container — a flowchart `subgraph` or a sequence `box` — holding its components. A flowchart node naming a contract or a boundary raises `SpecDiagramBoundary`.
  *P8*
- `node` — A flowchart node names one thing in at most 5 words: its label holds no `·`, `<br/>`, `,`, `;`, `: ` or error identifier, is no operation name, and sets no article second. A breach raises `SpecDiagramNode`.
  *because a box of bundled attributes or a verb hides the things and what flows between them; operations, limits and errors ride the edges*
- `shape` — A node labelled with a snake_case identifier or a phrase ending `table`, `store`, `log` or `queue` is a cylinder `[( )]`, and a label ending `?` is a decision `{ }`. A breach raises `SpecDiagramShape`.
  *because one shape meaning one kind lets a reader tell data from the parts acting on it at a glance*
- `edge` — A flowchart edge points one way; its label holds at most 5 words and no `·`, and an edge touching no decision node carries one. A breach raises `SpecDiagramEdge`.
  *because an unlabelled line says two things relate without saying how, and a two-way line hides which side acts*
- `decision` — A decision node asks a question or names a condition in at most 8 words and no error identifier, has an incoming edge, and has two or more outgoing edges each labelled with a distinct outcome. A breach raises `SpecDiagramDecision`.
  *because a branch without a guard on each exit leaves the reader to guess which path a case takes*
- `branch` — A non-decision node with an outgoing edge labelled `yes` or `no`, an outgoing error identifier beside another exit, or two exits opening `if`, `when`, `over`, `under` or `else` raises `SpecDiagramBranch`; a branch is a decision node.
  *because a condition hidden in edge labels off a box reads as parallel flow*
- `connected` — Every flowchart node, or a subgraph enclosing it, is an edge endpoint. A breach raises `SpecDiagramOrphan`.
  *because an unconnected element asserts nothing about the system*
- `unique-label` — No two nodes of one flowchart carry the same label; a second mention reuses the node id. A breach raises `SpecDiagramDuplicate`.
  *because one label naming two nodes leaves the reader to decide whether they are one thing*
- `layout` — A flowchart header declares `LR`, `TB` or `TD`, no subgraph declares `direction`, subgraphs nest at most 2 levels, and the chart holds at most 20 nodes and 25 edges. A breach raises `SpecDiagramLayout`.
  *because one reading direction keeps the flow followable, and a chart past that size or depth is two diagrams*
- `sequence` — A sequence diagram declares each participant it messages and messages each it declares, and holds at most 6 participants, 15 messages, 2 messages from a participant to itself and fragments nested 2 levels. A breach raises `SpecDiagramSequence`.
  *because a sequence diagram shows one scenario's interactions, and branching logic past that belongs in a flowchart*
- `message` — A sequence message label holds at most 8 words and a note at most 12 words. A breach raises `SpecDiagramMessage`.
  *because a message names one call; its parameters and bounds belong in clauses*
- `state` — A state diagram has one top-level `[*]` start reaching every state, every state has an outgoing transition, `[*]` counting as a target, and a transition label holds at most 6 words. A breach raises `SpecDiagramState`.
  *because an unreachable or dead-end state is a lifecycle the system cannot run*

## targets

Per-provider deployment target files, the roles each shape fills, and the page rendered from them.

- `file` — Each deployment provider's target profile is `spec/targets/<provider>.toml`: named shapes, exactly one marked default, each with a kind, the profiles it hosts, an optional per-invocation wall-clock cap, its exclusions and the primitive filling each role.
  *A-topology#build-profiles*
- `roles` — A shape fills eight roles — cron tick, reconciler, durable orchestrator, single-writer catalog, object store, hot-path query face, heavy compute and secrets — and may fill a realtime run projection and a queue.
- `primitives` — A target file lists the primitives its provider offers, and each shape lists the primitives it uses; {{topology.deploy.unsupported-shape}} refuses a shape using one outside that list.
  *because a shape's roles name products, and only a primitive list makes a missing one checkable*
- `incomplete` — A shape leaving a role unfilled, or a target file marking other than one default shape, raises `SpecTargetIncomplete`, naming the provider, the shape and the role.
  *because an unfilled role surfaces only when a deploy reaches it, on someone else's account*
- `cap-unrecorded` — A shape whose cap sits at or below the {{topology.deploy.wall-clock-cap}} bound and whose exclusions omit `first-time-backfill` or `component-connector` raises `SpecTargetCapUnrecorded`.
  *because the exclusion is the difference between a refused dispatch and a job killed mid-run*
- `function-profile` — A function-class shape listing a profile other than `edge` raises `SpecTargetFunctionProfile`, naming the provider and the shape.
  *because {{topology.package.edge-eligibility}} admits the edge profile alone on a function-class target*
- `page` — `contextful-spec state` renders `spec/targets.md` from the target files: per provider, a role table across its shapes and a diagram of its default shape.
  *because a diagram drawn by hand beside the data it shows drifts from it*

## guide

A non-normative teaching page per contract, and what keeps it from becoming a second home for a rule.

- `file` — Each contract has one guide, `spec/guide/<contract>.md`: front matter `contract`, one `# ` title equal to the contract's registry title, then prose outside diagrams of at most 400 words plus 20 words per operation its contract's fragment registers.
  *A-corpus#teaching-reference-live*
- `non-normative` — A guide holds no clause item and names no registered error. It reaches a rule by `{{<clause id>}}` and an operation by `{{<contract>.<operation>}}`, which resolves against that contract's fragment. No contract file links a guide.
  *because a rule restated in teaching prose is a second home that drifts from the first*
- `reach` — Each operation a contract's fragment registers appears in the contract's guide as an operation pointer or a pointer to one of its clauses; an operation the guide misses raises `SpecGuideReach`, naming it.
  *because a guide that skips an operation leaves its reader no route from the flow to that operation's rules*
- `pointer-density` — A guide paragraph, list item or table row holds at most 3 pointers to clauses and at least 8 words of prose per clause pointer; a denser one raises `SpecGuidePointerDensity`.
  *because a paragraph of pointers is an index without the reasoning that connects its entries, and the card already is the index*
- `bound` — A guide sentence holding a number equal to a bound value in its contract's fragment also holds the pointer to a clause owning a bound of that value; a sentence without one raises `SpecGuideBound`.
  *because a restated number drifts when its bound changes, and the pointer names the one place to change it*
- `bad-guide` — A contract without a guide, a guide breaking its shape or length, a guide holding a clause item or an error name, and a contract file linking a guide raise `SpecGuide`.
  *P8*

## topic

A non-normative page answering one question that spans contracts, the clauses it answers, and the bounds that keep it a route into the contracts.

- `file` — A topic page `spec/topics/<slug>.md` carries front matter `topic`, one `# ` title equal to its entry's `title` in `spec/terms/topic.toml`, then prose of at most 600 words outside diagrams.
  *because a topic routes a reader to the clauses, and a page past one screen of reading starts restating them*
- `pages` — The corpus holds at most 8 pages under `spec/topics/`.
  *because each topic answers a question the contracts split, and a growing set of them becomes a second corpus*
- `answers` — A topic entry lists under `answers` the clause ids its page answers; the page reaches each within one pointer step, by pointing at it or at a clause whose statement points at it.
  *because a reader of a topic follows one link to the rule, and a longer path is the search the page exists to save*
- `contract-pointers` — A topic page holds at most 3 pointers into any one contract, counting a repeated pointer once.
  *because a page pointing at most of one contract is that contract's guide, and a topic crosses contracts*
- `non-normative` — A topic page holds no clause item and names no registered error, reaching each rule by `{{id}}`; no contract file links a topic page.
  *because a rule restated on a topic page is a second home that drifts from the first*
- `bad-topic` — A page without an entry or an entry without a page, a page off its shape, words, pointer bound or {{corpus.topic.non-normative}}, more pages than {{corpus.topic.pages}}, and an unreachable or missing answer raise `SpecTopic`.
  *P8*
