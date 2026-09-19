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
  Records under [`spec/decisions/`](./spec/decisions/) — eight principles (`P1`–`P8`)
  and one record per decision several clauses share (`D01`…) — hold the options
  and costs, in at most 400 words each.
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
```

## Adding to the corpus

| Change | What it takes |
| --- | --- |
| A new fact | One clause row under the owning operation; a fragment entry for a new error, bound or shared term |
| A refusal | The clause, its error in the fragment, and a Why: a `because` cell, or a record id when two or more clauses share the decision |
| A new operation | A fragment entry, a `## <operation>` section, and its name in the file's `owns` |
| A new subject area | A contract entry in [`spec/terms/contract.toml`](./spec/terms/contract.toml), a fragment, then the file in the standard anatomy |
| Splitting a long file | Add a path to the contract's file list and move an `owns` entry. No clause id changes |

## Engineering conventions

[`spec/81-engineering.md`](./spec/81-engineering.md) is the sole home of how we
build: one home per capability, adapters rather than restatements, typed
automation, test placement, and the gate's resource budget.

## Writing

Affirmative present tense — the system does the thing, now, as a standing fact.
Every sentence carries a number, a name, a constraint or a refusal. No narration
of how a change was produced, in any file, at any time.
