# Working in this repository

The specification under [`spec/`](./spec/) is the source of truth, and
[`spec/00-corpus.md`](./spec/00-corpus.md) is the grammar it obeys. Read that
file before editing anything under `spec/`.

## The corpus rules, in short

- A normative sentence is one clause item, `` - `<subject>` — <statement> ``, at most
  40 words, in the list under its operation's `## ` heading and one-paragraph lede. Its
  address is `<contract>.<operation>.<subject>`: the file supplies the contract and the
  section the operation, both registered in [`spec/terms/`](./spec/terms/); the kind —
  refusal, limit, behavior — is computed.
- A fact has one home. The only second appearance is `{{<clause id>}}`. An error
  identifier and a numeric bound each belong to exactly one clause.
- A clause's Why, on the indented `*…*` line under it, carries a record id or a
  `because` of at most 30 words.
  Records under [`spec/adr/`](./spec/adr/) — eight principles (`P1`–`P8`, at most
  400 words each) and one ADR per contract (`A-store`, `A-run`, …), one section of at
  most 250 words per decision — hold the options and costs.
- No spec file says whether something is built, and none carries a date. Build
  state is computed into [`spec/status.md`](./spec/status.md) from
  [`spec/pins.toml`](./spec/pins.toml).
- An unknown is an inline `unsettled:` line in the section it affects.
- Each contract has a guide under [`spec/guide/`](./spec/guide/) that teaches the flow
  and reaches rules only by `{{id}}`, and a generated card under
  [`spec/cards/`](./spec/cards/) listing its operations, refusals and bounds.
- A diagram obeys `corpus.diagram`: a flowchart node is a noun of at most 5 words
  (tables and stores as cylinders, outside parties as stadiums, questions as `{ }`
  decisions), operations, limits and errors ride labelled one-way edges, and every
  decision exit names its outcome.
- Literature and practice live in [`references/`](./references/), which points into
  the spec by operation. The spec never cites.

`contextful-spec lint` implements every rule; the gate and a local run invoke the
identical command. A rule the checker cannot enforce is not a rule.

```sh
cargo run -q -p contextful-spec -- lint      # every rule
cargo run -q -p contextful-spec -- state     # regenerate spec/status.md, targets.md and cards/
cargo run -q -p contextful-spec -- extract   # regenerate spec/spec.lock.json
cargo run -q -p contextful-spec -- slice <target> [--json]
```

To hand one piece of work to an agent, give it `contextful-spec slice <target>`, where the
target is `<contract>.<operation>`, `<contract>.*` or a milestone number. The pack holds the
target's operation ledes and clauses, every clause their `{{id}}` pointers reach, the records
their Why lines cite, the errors and bounds they own, and a milestone's `Reach:` and `Acceptance:` lines.

## Adding to the corpus

| Change | What it takes |
| --- | --- |
| A new fact | One clause item under the owning operation; a fragment entry for a new error or bound |
| An example | A `- `<clause id>`: WHEN …, THEN …` item under the operation's `#### Scenarios`, or a `tests/fixtures/` path |
| A refusal | The clause, its error in the fragment, and a Why: a `because` line, or a record id when two or more clauses share the decision |
| A new operation | A fragment entry, a `## <operation>` section opening with its lede, and its name in the file's `owns` |
| A new subject area | A contract entry in [`spec/terms/contract.toml`](./spec/terms/contract.toml), a fragment, the file in the standard anatomy, and its guide |
| Splitting a long file | Add a path to the contract's file list and move an `owns` entry. No clause id changes |

## Test first, acceptance first

Every change to Rust source under `crates/` or `tools/` starts from a failing test.
[`A-assurance`](./spec/adr/A-assurance.md) records the decision;
the gate enforces it.

1. **Acceptance first.** Before pinning the first clause of a roadmap milestone, add the
   test its `Acceptance:` line names under `crates/acceptance/tests/integration/`, marked
   `#[ignore]` while the milestone is open. It drives a built binary through the CLI, MCP
   or HTTP surface and depends on no workspace package. A pin in a milestone with no
   acceptance test raises `SpecAcceptanceMissing`.
2. **Red.** Write the test under the package's `tests/integration/` and commit it with
   the change. The `test-first` stage runs the change's test files against the base
   commit's source and requires them to fail; a source change without one raises
   `TestNotFirst`. Inline `#[cfg(test)]` tests count toward the workspace stage, not
   toward this check.
3. **Green.** Implement until `cargo test --workspace` passes, then pin the clause to the
   test — an entry in `spec/pins.toml`, or a `// spec: <id>@<rev>` tag above the test
   function — and run `contextful-spec pins` to raise the floor. A pin to an `#[ignore]`d
   test, to a body still holding `todo!`, or through a tag whose rev no longer matches the
   statement computes `broken`. `contextful-spec scaffold <contract>.<operation> --package
   <path>` writes one tagged `todo!` test per refusal and limit clause to start from.
   **Proofs.** A clause the Lean models under `formal/` prove carries a theorem pin beside
   its test: `-- spec: <id>@<rev>` above the `theorem`, or a `theorem` entry in
   `spec/pins.toml`. `contextful-spec scaffold <contract>.<operation> --lean <file>`
   appends one tagged `sorry` theorem per clause, its statement as the docstring; a pinned
   theorem still holding `sorry` computes `broken`. The theorem proves the model; the test
   ties the model to the code; the clause performs when both do.
4. **Refactor.** A behavior-preserving commit carries the commit trailer
   `Test-First: refactor` and answers to the existing suite alone.
5. **Close the milestone** by removing the acceptance test's `#[ignore]`; status reports
   it `passing`, and `closed` once every operation it names holds a performed clause.

```sh
cargo run --locked -q -p contextful-ci -- gate                 # every stage, against origin/HEAD
cargo run --locked -q -p contextful-ci -- gate --stage test-first --base <rev>
```

The gate measures commits, so commit before running it.

## Fast Rust feedback

- Run the owning package's `tests/integration/` test while moving from red to green;
  use `cargo check -p <package>` for compiler feedback between test runs. Run
  `cargo test --workspace` before pinning a clause, as required above.
- Select only the features the current task exercises. For CLI work, start with
  `cargo check -p contextful-cli --no-default-features`, add `--features data-plane`
  for the data path, and enable `component-host` only for component-host work.
  For store work that does not serve reads, use
  `cargo check -p contextful-context --no-default-features` to omit DuckDB.
- The local Cargo wrapper pools target directories on the preferred mounted
  volume while it is writable, then falls back to the internal pool. Keep
  `CARGO_TARGET_DIR` unset; use `CARGO_SLOT_ROOT` only for isolated comparisons.
  Slot builds default to `CARGO_INCREMENTAL=0`; compare `CARGO_INCREMENTAL=1`
  for repeated edits to one package.
- Before changing build profiles, dependencies or gate caches, capture
  `cargo build --timings` for a representative feature set and compare a warm
  rebuild. Preserve the gate's separate stage targets and full feature coverage.

## The gate

The FlareDispatch GitHub App dispatches `contextful-ci gate` from same-repository
pull-request heads. Its `contextful-gate` run publishes the
`flare-dispatch/contextful-gate` parent and 24 child check-runs:
`flare-dispatch/check:<stage>` for each of `pins`, `toolchain`, `schema`, `test-first`,
`workspace`, `acceptance`, `evaluate`, `features`, `crate-graph`, `connectors`,
`surfaces`, `formal` and `budget`. The features and budget stages dispatch one check per
part, `flare-dispatch/check:features.<part>` and `flare-dispatch/check:budget.<profile>`, so
each fits the sandbox's wall clock; `contextful-ci stages --parts` prints the list, and
`--stage <stage>.<part>` runs one part. A selected subset runs in that order and refuses a
stage whose predecessor's output is absent; `--predecessors` runs those too. A local run and
the remote check invoke the identical command. `contextful-ci`'s suite asserts 24
dispatchable parts. FlareDispatch runs `contextful-measures` nightly against the default
branch and attaches its report to `refs/notes/measures`. A `v*` tag starts
`contextful-release`, with `contextful-release-cell` for the cells printed by
`contextful-ci release --plan`, `contextful-release-formula` for formulae and
SHA256SUMS, and three independently tagged container images. The disabled
ruleset proposal under `.github/rulesets/` lists the parent and all 24 children.

The sole Actions workflow transports FlareDispatch-admitted native Windows
execution through `workflow_dispatch` and read-only repository permission.
`contextful-ci native-transport` checks its fixed runners, immutable executor
checkout, bound inputs and artifact directory. FlareDispatch verifies authentic
API job conclusions and receipts before publishing native checks or artifacts.
The wrapper records subprocess evidence; reviewed workload code shares its user.

The schema stage also holds every key in a tracked `.env*` file to dotenvx ciphertext
under a comment stating what it grants (`contextful-ci secrets`); `.env.keys` stays
untracked. A deliberate restatement of an engine rule carries `mirrors: <clause id>` at
its site, and `contextful-ci mirrors` resolves each one. The pull-request template asks
the four boundary questions.

## Engineering conventions

[`spec/81-engineering.md`](./spec/81-engineering.md) is the sole home of how we
build: one home per capability, adapters rather than restatements, typed
automation, test placement, and the gate's resource budget.

## Writing

Affirmative present tense — the system does the thing, now, as a standing fact.
Every sentence carries a number, a name, a constraint or a refusal. No narration
of how a change was produced, in any file, at any time.
