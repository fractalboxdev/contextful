# 0172 — Every subject value is checked once, at the mint

**Status:** accepted 2026-09-18
**Decides:** `authority.identify.limit.subject-value`

## Context

A subject is a tuple of values — the agent, the host, the principal acted on behalf of,
the task scope, the zone. Those values are not inert. `on_behalf_of` is the key an
identity link joins against to decide which source rows a caller reaches; the authorizing
principal is stamped onto every landed row; the whole tuple is written into an audit
record and rendered wherever an admission is explained. A subject value is a database
key, a durable column, and display text, and it arrives from outside.

Each of those uses fails differently on a malformed value. An empty string joins against
a link table and matches whatever else is empty. A value carrying a control character
lands in an audit record and in operator output, where a line break lets one record's
text present as two. An unbounded value is a key of unbounded width in an index, a
column, and every audit row an admission produces.

Whitespace is the worst of them because it does not fail at all. A value with a trailing
space is a legal string everywhere. It mints, it lands, it renders identically to the
value without the space, and it joins against nothing the untrimmed value joins against —
so one person becomes two principals, one of whom reads nothing and one of whom reads
everything, and which one a request gets depends on where its claim template picked up
the space.

The mint is the single place every subject value passes through. Admission verifies a
signature over bytes already minted; a point of use reads a tuple already normalized.

## Decision

Every subject value is checked at the mint: non-empty, at most 256 B, carrying no control
character, and carrying no leading or trailing whitespace. The verified tuple is
normalized once — each value trimmed, each blank member dropped — before any consumer
reads it, and no point of use re-normalizes. A value that leaves the mint is a value
every consumer may use verbatim as a key, a column and rendered text.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Check and normalize once, at the mint** *(chosen)* | One identity has exactly one spelling everywhere it is read; every consumer treats the value as safe by construction | The bound and the character rule are fixed for every deployment, and a directory whose identifiers legitimately exceed them cannot be expressed |
| Treat the value as opaque and check at each point of use | Each consumer applies the rule its own use needs — a key check for the join, an escape for the render | Lost on consistency: two consumers normalize differently and one padded identity behaves as two, which is the failure the check exists to prevent |
| Check nothing; the value is whatever was signed | No rule to author, no refusal to register | Lost on containment: a control character reaches audit output and operator rendering, and an unbounded value reaches an index, from input the engine did not produce |
| Bound length far higher, or not at all | No legitimate identifier is ever turned away | Lost on cost: the value is a durable column on landed rows and on every audit record, so the bound is paid per row rather than per credential |
| Normalize silently instead of bounding | Nothing is refused; every value becomes usable | Lost on legibility: a truncated or rewritten identifier is a different principal, minted without anyone saying so |

## Criteria

1. **One identity, one spelling** — whether a single principal can be represented by two
   values that render identically.
2. **Containment** — whether an externally supplied value reaches a key, a column or
   rendered output in a form that misbehaves there.
3. **Cost per row** — what the bound buys given the value is stored on every landed row
   and every audit record.
4. **Expressiveness** — which real directory identifiers the rule turns away.

Criterion 1 decided it. Containment failures are loud: a control character in output or
an oversized key is noticed and traced back. Divergent spellings are silent and produce
wrong authorization rather than broken output — the affected principal reads nothing, or
reads under an identity nobody audited, and every layer involved reports success. A
failure mode that yields a wrong answer with no error outranks one that yields an error.

## Consequences

An identity link joins on a value it can compare directly, and audit output is safe to
render without per-consumer escaping. Adding a consumer of the subject tuple costs
nothing, because the guarantee travels with the value rather than with the reader.

The cost accepted is that the hygiene rules are the mint's and not the deployment's. A
directory whose identifiers exceed 256 B, or that meaningfully distinguishes two values
differing only in surrounding whitespace, cannot be represented at all. The bound is
asserted rather than measured against a survey of real directory identifiers; the
headroom over the longest identifier a deployment actually carries is unknown.

Raising the bound later is cheap. Lowering it is not — it invalidates credentials already
minted and identities already stamped onto landed rows.

## Revisit triggers

- A deployment's identity provider issues a principal identifier that does not fit, or
  carries a character class the rule refuses.
- A consumer of the subject tuple is found normalizing a value it reads, which means the
  single-normalization guarantee has stopped holding.
- The subject value stops being a durable column on landed rows, which removes the
  per-row cost that sets the bound.
