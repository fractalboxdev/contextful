# 0024 — A valid-time-bounded read refuses a table that declares no valid-time pair

**Status:** accepted 2026-09-18
**Decides:** `store.bound-time.refusal.undeclared-valid-time`

## Context

A row carries two clocks. Transaction time is the injected ingest stamp: the engine wrote
the row, so that instant is engine knowledge and correct by construction on every table.
Valid time is a pair of the table's own columns, in the source's vocabulary, declared per
table — it says when the fact the row states was true in the world, which only the source
knows.

The two bounds read differently and answer different questions. A transaction-time bound
answers what the store knew at an instant; a valid-time bound answers what was true at an
instant. They agree only where ingestion was prompt and nothing was restated, and they
disagree for exactly the rows a reader asks a valid-time question about — a correction
landed late, a fact backdated, a record restated after the period it describes.

The bound is also used where being wrong is expensive. A valid-time read decides whether a
reconstruction of a past state is admissible: what the world looked like then, with no
knowledge acquired afterwards folded in. A bound that silently answered a different
question would produce a reconstruction that is defensible-looking and includes facts
recorded later.

Inferring the pair is the tempting alternative and is a guess in the literal sense: a
heuristic picks a date-like column out of the source's schema. Where the guess is wrong the
same column is also what ranking orders by, so the cost is paid twice — once in the answer
and once in what the answer omits — and neither failure is visible in the result.

## Decision

A `valid_as_of` read against a table carrying no declared pair raises
`StoreValidTimeUndeclared` and names the table, rather than answering the transaction-time
question in its place. A table declares its clock columns explicitly, with a required
`from` and an optional `to`, and the engine infers neither the pair nor stamps a second
column claiming to be the first.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse the bound on an undeclared table** *(chosen)* | A valid-time answer is grounded in columns the source declared, or there is no answer | A table earns the bound by declaring it, which is a manifest change and friction on adoption |
| Answer the transaction-time question in its place | Every table supports every bound; no read ever refuses | Lost on admissibility: the two clocks disagree for exactly the rows the reader is asking about, so the substitution is wrong precisely where the question matters |
| Infer the pair from a heuristic date-column picker | Most tables get the bound with no authoring | Lost on admissibility: a wrong guess costs ordering in ranking and costs the answer as a bound, and neither loss is visible in the result |
| Inject a second engine column beside the ingest stamp | Uniform, always present, needs no declaration | Lost on honesty: it stamps ingestion under a name claiming otherwise, which makes every table appear to support a bound none of them actually carries |

## Criteria

1. **Admissibility** — whether an examiner can rely on what a bounded read returns. *(the
   one that decided it)* A bound deciding whether a reconstruction is admissible cannot
   rest on a guess or on a substituted clock, and the substitution is wrong exactly for the
   rows in question. Coverage — how many tables support the bound without authoring — was
   the competing criterion and lost, because an unsupported bound is a refusal an author
   resolves in one manifest change, while an unsound bound is an answer nobody can
   challenge from the result alone.
2. **Which clock the engine can honestly know** — the engine wrote the row and therefore
   knows transaction time; only the source knows when a fact became true.
3. **Visibility of a wrong inference** — whether a bad guess shows up in the result.
4. **Coverage** — how many tables answer a valid-time question with no authoring.

## Consequences

A valid-time answer rests on columns the source declared, and a table that cannot answer
the question says so by name rather than returning something else. Transaction-time bounds
stay universally available, since the engine wrote the stamp.

The cost accepted is adoption friction: a table earns the bound by declaring it, which is a
manifest change per table, and until that change lands the bound refuses. A deployment that
wants bitemporal reads broadly pays that per table rather than once.

Declaring the pair also changes the table's shape. A keyed table with a declared valid-time
pair holds more than one live row per key by design, since the fold partitions on the key
together with the valid-time line and keeps one row per line — so declaring the pair is not
a read-only annotation, and a consumer expecting one row per key sees more.

Over an append-only table a valid-time bound returns every version whose interval covers
the instant, because nothing has decided which of two covering rows supersedes the other.
Choosing between them is what declaring a key and folding does.

## Revisit triggers

- Source manifests begin carrying a standard valid-time annotation the engine can read
  directly, which turns inference into a declaration rather than a guess.
- The fraction of tables declaring the pair stays near zero across deployments that want
  bitemporal reads, indicating the authoring cost, not the honesty argument, is what is
  binding.
- A use appears for a bound over a clock the engine can know that is neither the ingest
  stamp nor a source column, which would add a third clock rather than re-open this one.
