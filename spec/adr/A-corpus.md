# A-corpus — Corpus grammar decisions

**Status:** accepted

## clause-addressed-sentence — A clause is an addressed sentence in a list, not a table row

The id stays on the sentence, as a margin anchor does, and a reader meets the operation's purpose before its first rule. The full id still reaches the lock file, the pins and every `{{id}}`.

| Option | Lost on | Cost |
| --- | --- | --- |
| Subject-anchored list items under a lede *(chosen)* | — | No single source line holds a full id; a subject rename is still an id change. |
| Three-column clause table | Readability | The id prefix repeats on every row, and no operation has an introduction. |
| Prose paragraphs with content-hash ids | Stable addressing | Any wording edit renames the clause, so every pin and pointer churns. |
| Prose paragraphs with random stable ids | Meaningful ids | A test tag names an opaque token instead of the rule it demonstrates. |

## teaching-reference-live — Teaching and reference live on separate pages

Contract files are reference; a guide teaches the flow in plain language, and a card gathers operations, error cases and bounds on one page.

| Option | Lost on | Cost |
| --- | --- | --- |
| Guide by pointer, generated card *(chosen)* | — | One guide per contract to keep current within its word cap. |
| Explanatory prose inside contract files | One home per fact | Teaching sentences restate rules and drift from them. |
| No guide, card only | Understanding | An index of error cases does not explain the model or the flow. |
