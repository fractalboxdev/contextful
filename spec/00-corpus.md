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
  - targets
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
  pins.toml             clause id -> demonstrating artifact, plus the coverage floor
  roadmap.md            milestone -> operation set
  status.md             generated
  spec.lock.json        generated
references/             literature and practice; points into spec/, never the reverse
tools/spec/             the checker
formal/                 Lean 4 models and theorems, pinned like tests
```

## address

| Clause | Statement | Why |
| --- | --- | --- |
| `corpus.address.clause-id` | A clause id is three dot-separated segments — contract, operation, subject — each matching `[a-z0-9-]+`, written backticked in the first cell of a clause row. | P8 |
| `corpus.address.contract-segment` | The first segment equals the file's front-matter `contract`, which names an entry of `spec/terms/contract.toml` listing that file. | P8 |
| `corpus.address.operation-segment` | The second segment is an operation registered in the contract's fragment and listed in the front-matter `owns` of exactly one of the contract's files, the file carrying the row. | P8 |
| `corpus.address.subject-segment` | The third segment is a slug of at most 40 chars, unique within its operation. It is a name, not a registry entry. | P8 |
| `corpus.address.ids-are-stable` | An id changes when a fact changes obligor. Moving an operation between two files of one contract rewrites no id. | P8 |
| `corpus.address.malformed-id` | A clause id violating the shape, naming another contract, or naming an operation its file does not own raises `SpecMalformedId`. | P8 |
| `corpus.address.duplicate-id` | A clause id appearing in two rows raises `SpecDuplicateId`, naming both locations. | P8 |

## anatomy

| Clause | Statement | Why |
| --- | --- | --- |
| `corpus.anatomy.file-headings` | A contract file carries front matter with `contract` and `owns`, one `# ` title matching its contract entry, one `## <operation>` section per owned operation in `owns` order, then an optional `## Shapes`. | P8 |
| `corpus.anatomy.clause-table` | An operation section holds one table headed `Clause`, `Statement`, `Why`; each row is one clause. Prose, diagrams, scenarios and unsettled lines may follow the table. | P8 |
| `corpus.anatomy.scenario` | A `#### Scenarios` list under an operation holds items `` - `<clause id>`: WHEN <trigger>, THEN <outcome> `` or a backticked `tests/fixtures/` path; the lock file attaches each to its clause. | because an example is the test an implementer writes first, and a rule without one leaves the agent to invent it |
| `corpus.anatomy.bad-scenario` | A scenario naming a clause outside its operation, or holding neither a WHEN/THEN sentence nor a fixture path, raises `SpecScenario`. | P8 |
| `corpus.anatomy.kind-is-computed` | A clause is a refusal when its statement names an error its fragment assigns to it, a limit when its fragment assigns it a named bound, and a behavior otherwise. The checker writes the kind into `spec/spec.lock.json`. | P8 |
| `corpus.anatomy.statement-words` | A clause statement holds at most 40 words, a `{{id}}` reference counting as one. | because one row states one obligation; a longer row is two clauses or carries rationale |
| `corpus.anatomy.file-length` | A contract file holds at most 900 lines. | because a file past that splits by adding a path to its contract entry, which renames nothing |
| `corpus.anatomy.bad-anatomy` | A missing or extra `##` section, an out-of-order section, a table with other headers, a title differing from the registry, a file over its length or a statement over its word count raises `SpecAnatomy`. | P8 |

## registry

| Clause | Statement | Why |
| --- | --- | --- |
| `corpus.registry.fragment` | Each contract owns `spec/terms/<contract>.toml`, holding its operations, errors and named bounds, each with a gloss. | P8 |
| `corpus.registry.one-error-one-clause` | An error entry names the one clause raising it. That clause's statement names the error, and no other clause statement does; a second site reaches the condition by `{{id}}`. | P2 |
| `corpus.registry.bound-entry` | A named-bound entry carries its owning clause, a value, a unit from `spec/terms/unit.toml` and a basis: `chosen`, `measured:<benchmark>` or `standard:<name>`. The owning statement carries the value followed by the unit. | because an unmeasured number presented as a measurement is a hypothesis stated as fact |
| `corpus.registry.unregistered` | An unregistered error after `raises`, an entry naming a missing clause, an error absent from its clause or present in another, and a malformed bound raise `SpecRegistry`. | P8 |

## reference

| Clause | Statement | Why |
| --- | --- | --- |
| `corpus.reference.pointer` | `{{<clause id>}}` is the one way a statement reaches a fact another clause owns. The lock file records every pointer as an edge. | P8 |
| `corpus.reference.dangling` | A `{{id}}` naming no clause raises `SpecDanglingReference`. | P8 |
| `corpus.reference.no-literature` | A spec file carries no hyperlink to an external document and no path under `references/`; either raises `SpecExternalLink`. Literature lives in `references/`, which cites clause ids. | because the contract is judged by behavior, and a citation invites arguing from authority |

## rationale

| Clause | Statement | Why |
| --- | --- | --- |
| `corpus.rationale.why-cell` | A Why cell is empty, `—`, one or more record ids such as `P3` or `A-store`, or `because` followed by at most 30 words stating the deciding criterion. A refusal row carries a non-empty Why. | P8 |
| `corpus.rationale.record-threshold` | A decision earns a section in its contract's ADR when two or more clauses depend on it, it spans contracts, or it names a revisit trigger; otherwise it lives in a `because` cell. | P8 |
| `corpus.rationale.record-anatomy` | A principle `P<n>-<slug>.md` carries `# <id> — <title>`, a `**Status:**` line, then Context (optional), Decision, Options, Consequences, Revisit (optional), in at most 400 words. | because a principle argues one rule every contract obeys, and the length of the argument is part of its quality |
| `corpus.rationale.contract-adr` | A contract's decisions live in one `A-<contract>.md`: `# A-<contract> — <title>`, a `**Status:**` line, then one `## <decision>` section per decision, each holding its options table in at most 250 words. | because one file per contract puts every decision beside the clauses it governs, and a fresh corpus has no history to keep apart |
| `corpus.rationale.options-table` | A principle's Options and each ADR section hold one table headed `Option`, `Lost on`, `Cost` with two to five rows. One row is marked *(chosen)* with `—` as Lost on; every other row names the criterion it lost on. | P8 |
| `corpus.rationale.unsettled-line` | An unknown is one line where it applies: `unsettled:`, a question ending `?`, `owner:` and a handle, `affects:` and a `<contract>.<operation>`. Another form, or a heading `Open questions`, `Out of scope` or `See also`, raises `SpecUnsettled`. | P8 |
| `corpus.rationale.orphan-record` | A record no Why cell cites, a Why cell naming no record, and a record breaking its anatomy raise `SpecRecord`. | P8 |

## state

| Clause | Statement | Why |
| --- | --- | --- |
| `corpus.state.status-is-computed` | `spec/status.md` is the sole statement of which clauses the tree demonstrates. No authored file says whether something is built. | P8 |
| `corpus.state.pin` | `spec/pins.toml` maps a clause id to one artifact: a `test` function path, a `theorem` constant under `formal/` or an `item` path. A refusal or a limit takes a test or a theorem. | P8 |
| `corpus.state.verdict` | An unpinned clause computes `committed`; a pinned clause computes `performed` when its artifact's final path segment is defined under `crates/`, `tools/` or `formal/`, and `broken` otherwise. A test carrying an `#[ignore]` attribute computes `broken`. | P8 |
| `corpus.state.tag-pin` | A line `// spec: <id>@<rev>` among the comments and attributes above a test function under `crates/` or `tools/` pins clause `<id>` to that test; `<rev>` is the first 8 hex digits of the statement's SHA-256. | because a pin written beside its test travels with every move and rename, and the digest records which wording the test demonstrates |
| `corpus.state.lean-tag` | A line `-- spec: <id>@<rev>` above a Lean `theorem` or `lemma` under `formal/`, past comments, docstrings and attributes, pins clause `<id>` to that theorem, with `<rev>` as in {{corpus.state.tag-pin}}. | because a theorem's pin then moves with the proof, and a reworded clause marks the proof stale |
| `corpus.state.theorem-beside-test` | A clause carries at most one theorem pin and one test pin, and computes `performed` when both perform: the theorem proves the model, the test ties the model to the code. | A-assurance |
| `corpus.state.stale-pin` | A tag whose `<rev>` differs from the digest of its clause's current statement raises `SpecStalePin`, and its clause computes `broken`. | because a reworded clause no longer says what its test was written to demonstrate |
| `corpus.state.unfinished-test` | A pinned test holding a body line that opens with `todo!`, and a pinned Lean declaration holding `sorry` or `admit`, compute `broken`. | because a placeholder demonstrates nothing, whether it panics at run time or proves by assumption |
| `corpus.state.scaffold` | `contextful-spec scaffold <contract>.<operation> --package <path>` writes one tagged test per refusal and limit clause of the operation into `<path>/tests/integration/`, its body a `todo!` naming the error or bound, and rewrites no existing function. | |
| `corpus.state.scaffold-lean` | `contextful-spec scaffold <contract>.<operation> --lean <file>` appends one tagged theorem per clause of the operation, its statement the docstring and its proposition and proof `sorry`, and rewrites no existing theorem. | |
| `corpus.state.coverage-floor` | `spec/pins.toml` carries a per-contract floor of pinned clauses. A live count below its floor raises `SpecCoverageRegression`. | because deleting a failing pin must not read as progress |
| `corpus.state.bad-pin` | A pin naming no clause, a tag above no declaration, an item pin on a refusal or limit, a broken pin, and one clause pinned to two tests or two theorems raise `SpecBrokenPin`. | P8 |
| `corpus.state.roadmap` | `spec/roadmap.md` names operations as `<contract>.<operation>` or `<contract>.*`. A name resolving to no operation, an operation claimed by two milestones, or a milestone lacking its `Reach:` or `Acceptance:` line raises `SpecRoadmap`. | P8 |
| `corpus.state.acceptance` | A milestone's `Acceptance:` line names one test under `crates/acceptance/`; that test computes `absent` when undefined, `open` when ignored, and `passing` otherwise. | A-assurance |
| `corpus.state.acceptance-first` | A milestone holding a pinned clause while its acceptance test computes `absent` raises `SpecAcceptanceMissing`. | A-assurance |
| `corpus.state.deferred-depth` | A milestone carrying a `Depth: operation` line admits only refusal and limit clauses; a behavior clause of an operation it schedules raises `SpecDeferredBehavior`. | because an unopened milestone fixes what it refuses and bounds, and its behavior is written against the code that opens it |

## render

| Clause | Statement | Why |
| --- | --- | --- |
| `corpus.render.generated` | `spec/status.md`, `spec/targets.md` and `spec/spec.lock.json` are regenerated by `contextful-spec state` and `contextful-spec extract`. A committed copy differing from regeneration raises `SpecStaleRender`. | P8 |
| `corpus.render.lock-file` | The lock file carries every clause with its segments, computed kind, statement, Why and location, the per-file `owns` map, the registry and the pointer graph. | P8 |
| `corpus.render.dated-prose` | The words `planned`, `not yet`, `currently`, `today`, `shipped`, `implemented`, `previously`, `used to`, `legacy`, `TODO`, `FIXME` and `WIP`, and an ISO date, raise `SpecDatedProse` in any authored file. | because build state is computed, and a dated sentence is stale the day it lands |
| `corpus.render.counterfactual` | The words `will`, `would` and `shall` raise `SpecCounterfactual` in a contract file. | because a contract states present behavior, and a record's Options is where a path not taken is described |
| `corpus.render.banned-vocabulary` | The nouns `seam`, `load-bearing`, `wedge`, `rung` and `land-grab`, a word whose digest `spec/terms/refused-names.toml` lists, a bare `#<digits>` and a pull-request link raise `SpecBannedWord`. | P8 |
| `corpus.render.local-path` | A path beginning `/Users/`, `/home/`, `$HOME/` or `~/` raises `SpecLocalPath`. | P8 |
| `corpus.render.diagram` | A diagram is a fenced `mermaid` block; box-drawing characters outside a fence raise `SpecAsciiDiagram`. | P8 |


## targets

| Clause | Statement | Why |
| --- | --- | --- |
| `corpus.targets.file` | Each deployment provider's target profile is `spec/targets/<provider>.toml`: named shapes, exactly one marked default, each with a kind, the profiles it hosts, an optional per-invocation wall-clock cap, its exclusions and the primitive filling each role. | A-topology |
| `corpus.targets.roles` | A shape fills eight roles — cron tick, reconciler, durable orchestrator, single-writer catalog, object store, hot-path query face, heavy compute and secrets — and may fill a realtime run projection. | — |
| `corpus.targets.incomplete` | A shape leaving a role unfilled, or a target file marking other than one default shape, raises `SpecTargetIncomplete`, naming the provider, the shape and the role. | because an unfilled role surfaces only when a deploy reaches it, on someone else's account |
| `corpus.targets.cap-unrecorded` | A shape whose cap sits at or below the {{topology.deploy.wall-clock-cap}} bound and whose exclusions omit `first-time-backfill` or `component-connector` raises `SpecTargetCapUnrecorded`. | because the exclusion is the difference between a refused dispatch and a job killed mid-run |
| `corpus.targets.function-profile` | A function-class shape listing a profile other than `edge` raises `SpecTargetFunctionProfile`, naming the provider and the shape. | because {{topology.package.edge-eligibility}} admits the edge profile alone on a function-class target |
| `corpus.targets.page` | `contextful-spec state` renders `spec/targets.md` from the target files: per provider, a role table across its shapes and a diagram of its default shape. | because a diagram drawn by hand beside the data it shows drifts from it |

A clause row and an unsettled line read:

```markdown
| `store.fold.partial-snapshot` | A reader observes a snapshot and every declared sidecar together or neither; a commit exposing one without the other raises `StorePartialSnapshot`. | P4 |

unsettled: Does a replica that has never pulled report an empty store or refuse the read? owner: read-path affects: read.serve
```

