# The registry

The controlled vocabulary. `spec/00-corpus.md` § registry states the rules; this file
says where each spelling lives.

| File | Holds |
| --- | --- |
| `contract.toml` | The ten contracts, each with its ordered file list and the title every file's `# ` heading matches |
| `<contract>.toml` | One contract's operations, errors, named bounds and shared terms |
| `unit.toml` | The unit tokens a bound carries |
| `wire.toml` | Vocabulary a standard defines — a header, a status code, a media type — which no contract owes |
| `refused-names.toml` | Names the corpus does not carry, stored as SHA-256 digests so the denylist does not print them |

A contract owns its fragment, so two people adding vocabulary to two contracts do not
meet. `contextful-spec lint` reads the fragments directly; nothing is derived from them.

| Adding | Where | Also |
| --- | --- | --- |
| An operation | `[operation]` | A `## <operation>` section in exactly one of the contract's files, and its name in that file's `owns` |
| An error | `[error]` with `clause` | The named clause's statement raises it, and no other statement names it |
| A bound | `[limit]` with `clause`, `value`, `unit`, `basis` | The owning statement carries `<value> <unit>` |
| A term | `[term]` | Only for an identifier appearing in clause statements of two or more contracts |
| A contract | `contract.toml`, then a fragment | The file itself in the standard anatomy |
