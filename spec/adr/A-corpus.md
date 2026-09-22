# A-corpus — Corpus grammar decisions

**Status:** accepted

## A clause is an addressed sentence in a list, not a table row

`corpus.anatomy` writes each clause as a list item, `` - `<subject>` — <statement> ``, under its operation's heading, with its Why on the following line. The contract and operation segments come from the file and the section, so the source carries the subject alone and the full id still lands in the lock file, the pins and every `{{id}}`. Each operation opens with a lede that is its only description. The id stays on the sentence, the way a margin anchor does, and a reader meets the operation's purpose before its first rule.

| Option | Lost on | Cost |
| --- | --- | --- |
| Subject-anchored list items under a lede *(chosen)* | — | A full id appears in no single line of source; finding one takes its section heading or the lock file. |
| Three-column clause table | Readability | The id prefix repeats on every row, reasons are squeezed into a cell, and no operation has an introduction. |
| Tables in source, prose only in the rendered site | Source readability | Authors and reviewers read the source, and a diff of the source is what a pull request shows. |
| Prose paragraphs with content-hash ids | Stable addressing | Any edit to the wording renames the clause, so every pin and pointer to it churns. |
| Prose paragraphs with random stable ids | Meaningful ids | A test tag names an opaque token instead of the rule it demonstrates. |

Consequences: `slice` and the lock file keep their shape; a subject rename is still an id change.

## Teaching and reference live on separate pages

Contract files are reference, and each contract also has a guide under `spec/guide/` that teaches the flow and reaches rules only by `{{id}}`. A card generated per contract gathers the operations, refusals and bounds on one page.

| Option | Lost on | Cost |
| --- | --- | --- |
| Guide by pointer, generated card *(chosen)* | — | Ten guides to keep current; a guide over its length or naming an error fails the gate. |
| Explanatory prose inside contract files | One home per fact | Teaching sentences restate rules and drift from the rules they restate. |
| No guide, card only | Understanding | An index of refusals does not teach the model those refusals protect. |
