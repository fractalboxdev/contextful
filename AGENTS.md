# Working in this repository

The specification under [`spec/`](./spec/) is the source of truth, and
[`spec/00-corpus.md`](./spec/00-corpus.md) is the grammar it obeys. Read that
file before editing anything under `spec/`.

## The corpus rules, in short

- A normative sentence is one clause row, at most 40 words, addressed
  `<contract>.<operation>.<subject>`. The contract and operation are registered in
  [`spec/terms/`](./spec/terms/); the kind — refusal, limit, behavior — is computed.
- A fact has one home. The only second appearance is `{{<clause id>}}`. An error
  identifier and a numeric bound each belong to exactly one clause.
- A clause's Why cell carries a record id or a `because` of at most 30 words.
  Records under [`spec/adr/`](./spec/adr/) — eight principles (`P1`–`P8`, at most
  400 words each) and one ADR per contract (`A-store`, `A-run`, …), one section of at
  most 250 words per decision — hold the options and costs.
- No spec file says whether something is built, and none carries a date. Build
  state is computed into [`spec/status.md`](./spec/status.md) from
  [`spec/pins.toml`](./spec/pins.toml).
- An unknown is an inline `unsettled:` line in the section it affects.
- Literature and practice live in [`references/`](./references/), which points into
  the spec by operation. The spec never cites.

`contextful-spec lint` implements every rule; the gate and a local run invoke the
identical command. A rule the checker cannot enforce is not a rule.

```sh
cargo run -q -p contextful-spec -- lint      # every rule
cargo run -q -p contextful-spec -- state     # regenerate spec/status.md
cargo run -q -p contextful-spec -- extract   # regenerate spec/spec.lock.json
cargo run -q -p contextful-spec -- slice <target> [--json]
```

To hand one piece of work to an agent, give it `contextful-spec slice <target>`, where the
target is `<contract>.<operation>`, `<contract>.*` or a milestone number. The pack holds the
target's clause rows, every row their `{{id}}` pointers reach, the records their Why cells
cite, the errors and bounds they own, and a milestone's `Reach:` and `Acceptance:` lines.

## Adding to the corpus

| Change | What it takes |
| --- | --- |
| A new fact | One clause row under the owning operation; a fragment entry for a new error, bound or shared term |
| A refusal | The clause, its error in the fragment, and a Why: a `because` cell, or a record id when two or more clauses share the decision |
| A new operation | A fragment entry, a `## <operation>` section, and its name in the file's `owns` |
| A new subject area | A contract entry in [`spec/terms/contract.toml`](./spec/terms/contract.toml), a fragment, then the file in the standard anatomy |
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
   test in `spec/pins.toml` and run `contextful-spec pins` to raise the floor. A pin to
   an `#[ignore]`d test computes `broken`.
4. **Refactor.** A behavior-preserving commit carries the commit trailer
   `Test-First: refactor` and answers to the existing suite alone.
5. **Close the milestone** by removing the acceptance test's `#[ignore]`; status reports
   it `passing`.

```sh
cargo run -q -p contextful-ci -- gate                          # every stage, against origin/HEAD
cargo run -q -p contextful-ci -- gate --stage test-first --base <rev>
```

The gate measures commits, so commit before running it.

## The gate

[`.github/workflows/gate.yml`](./.github/workflows/gate.yml) dispatches each stage of
`contextful-ci gate` to the org's FlareDispatch Dispatcher as a `check` run. Each stage
reports as its own check-run, and branch protection requires all four:
`flare-dispatch/check:schema`, `flare-dispatch/check:test-first`,
`flare-dispatch/check:workspace` and `flare-dispatch/check:acceptance`. A local run and
the remote check invoke the identical command; `contextful-ci`'s suite fails when the
workflow's stage matrix and the subcommand's stage list differ.

## Engineering conventions

[`spec/81-engineering.md`](./spec/81-engineering.md) is the sole home of how we
build: one home per capability, adapters rather than restatements, typed
automation, test placement, and the gate's resource budget.

## Writing

Affirmative present tense — the system does the thing, now, as a standing fact.
Every sentence carries a number, a name, a constraint or a refusal. No narration
of how a change was produced, in any file, at any time.
