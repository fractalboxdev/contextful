# The registry

The controlled vocabulary. A contract, an operation, a subject, a unit, an error
identifier and a named bound each carry one spelling, recorded here and nowhere
else. `spec/00-corpus.md` § Clauses — registry states the rules; this file says
where each spelling lives and how to add one.

| File | Holds | Authored or derived |
| --- | --- | --- |
| `contract.toml` | The twenty contracts, each with its ordered file list and the title every file's `# ` heading matches | authored |
| `scope.toml` | Every check's path globs and exemptions, and each path's role | authored |
| `unit.toml` | The unit tokens a `limit` clause may carry | authored |
| `fragments/<contract>.toml` | One contract's operations, subjects, error identifiers and named bounds | authored — this is where you edit |
| `operation.toml`, `term.toml`, `error.toml`, `limit.toml` | The flat registries every check reads | derived from `fragments/` |

A contract owns its fragment, so two people adding vocabulary to two contracts do
not meet. The four flat files are generated:

```sh
python3 tools/spec/merge-registry.py           # rewrite them from the fragments
python3 tools/spec/merge-registry.py --check   # the gate's form — fails on any drift
```

The merge refuses a spelling two fragments both claim, naming both contracts. That
refusal is the point: one spelling names one thing, and two contracts reaching for
the same word is the moment to decide whether they mean one thing — in which case
the obligor keeps it and the other reaches for it — or two, in which case one of
them renames.

`fragments/wire.toml` is not a contract. It carries vocabulary a standard defines —
a header, a status code, a media type — which no party answers for, so no clause is
addressed on one and the usage-to-owner join passes over it.

## Adding a spelling

| Adding | Where | Also |
| --- | --- | --- |
| A subject | `[term]` in your contract's fragment | Give it a gloss, and `resolves = "code"` when the token names a Rust item, a field, an error variant, a command verb, a config key or an environment read |
| An error identifier | `[error]` in your fragment | Every `refusal` clause names one |
| A named bound | `[limit]` in your fragment | `owner` is the clause id asserting it, and the numeral in that clause resolves here |
| An operation | `[operation]` in your fragment | Plus a decision record citing it — minting a verb is recorded, never silent |
| A contract | `contract.toml`, then a fragment | Plus the file itself in the standard anatomy |
