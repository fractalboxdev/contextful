# Working in this repository

The specification under [`spec/`](./spec/) is the source of truth, and
[`spec/00-corpus.md`](./spec/00-corpus.md) is the grammar it obeys. Read that
file before editing anything under `spec/`.

## The corpus rules, in short

- A normative sentence is one clause row addressed `<contract>.<operation>.<kind>.<subject>`.
  Three of those four coordinates are looked up in [`spec/terms/`](./spec/terms/),
  so where a sentence belongs is a lookup rather than a judgment.
- A fact has one home. The only second appearance is `{{<clause id>}}`, which the
  renderer inlines — the copy is generated, so it cannot drift.
- Rationale lives under [`spec/decisions/`](./spec/decisions/), one record per
  decision, and nowhere else. A contract file states behavior and names no
  alternative.
- No spec file says whether something is built, and none carries a date. Build
  state is computed into [`spec/status.md`](./spec/status.md) from
  [`spec/pins.toml`](./spec/pins.toml).
- An unknown is an inline `unsettled:` line in the section it affects, carrying a
  question, an owner and the operation it affects. There is no appendix for them.

`contextful spec lint` implements every rule; the gate and a local run invoke the
identical command. A rule the checker cannot enforce is not a rule.

## Adding to the corpus

| Change | What it takes |
| --- | --- |
| A new fact in an existing subject | One clause row under the owning operation, plus a registry entry for any new backticked token, unit or error identifier |
| A new subject area | A contract entry in [`spec/terms/contract.toml`](./spec/terms/contract.toml) with its file list and title, then the file itself in the standard anatomy |
| A new operation | A registry entry plus a decision record citing it — minting a verb is recorded, never silent |
| A refusal whose direction is a choice | The clause, plus the record its `decided-by` cell names |
| Splitting a file that got long | Add a second path to the contract's file list and move an `owns` entry. No clause id changes |

## Engineering conventions

[`spec/61-engineering.md`](./spec/61-engineering.md) is the sole home of how we
build: one home per capability, adapters rather than restatements, typed
automation over compiled binaries, test placement, and the gate's resource
budget.

## Writing

Affirmative present tense — the system does the thing, now, as a standing fact.
Every sentence carries a number, a name, a constraint or a refusal. No narration
of how a change was produced, in any file, at any time.
