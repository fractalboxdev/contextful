# 0270 — A template's statement runs unrewritten, and its identifiers are confined to plain store table names

**Status:** accepted 2026-09-18
**Decides:** `disclosure.template.refusal.plain-identifier`

## Context

Two kinds of statement reach the engine and they have different authors. A caller-written
statement arrives from a principal the deployment does not trust and passes a guard that
decides which shapes are expressible at all. A template's statement is written by the
operator, read by a reviewer, and fixed in the manifest before any caller exists.

A whitelisted template exists precisely to express a read the caller-facing guard refuses. An
operator declares one when the metric needs a shape — a particular join, a window, a function
the guard does not admit — that is safe because a human read it and unsafe as a general
capability. Putting that statement back through the guard removes the reason the mechanism is
there: the operator would be gated on their own reviewed text, and the templates that survive
would be exactly the statements a caller could have written.

Running unrewritten is not the same as running unenforced. The allowlist, row restriction,
column masking and placement resolution all apply to whatever the template touches, because
those layers act on resolved relations rather than on statement text. What the template does
control is which relations it resolves to, and that is where the exposure sits. A statement
that names a file path directly, a table function that opens bytes by argument, or a qualified
catalog reaching outside the store's own namespace all resolve to something the enforcement
layers were never given a chance to filter — the tenant filter narrows tables, and a path is
not a table.

So the confinement has to sit on the identifiers rather than on the shapes. A template names
store tables as plain identifiers and nothing else: no table functions, no bare paths, no
qualified catalogs. A plain identifier may carry a path separator, and carries no dot, no star
and no leading separator.

That check reads only the declaration. It does not depend on who calls, what arguments arrive,
or which grants are in force, which means it has one correct evaluation time: once, at startup,
alongside the rest of template validation.

## Decision

A template's statement runs unrewritten, and every enforcement layer still applies to what it
touches — the allowlist, row restriction, column masking and placement resolution. A template
naming anything other than a store table as a plain identifier raises
`DisclosureTemplateNonPlainIdentifier`: no table functions, no bare paths, no qualified
catalogs. A plain identifier may carry a path separator, and carries no dot, no star and no
leading separator. The check is caller-independent and runs once at startup.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Unrewritten execution, identifiers confined to plain store tables, checked at startup** *(chosen)* | The operator's reviewed statement runs as written while every relation it touches stays inside the filtered namespace. One evaluation covers every future call. | A store supporting table functions cannot expose them through templates, and the identifier confinement is the whole protection on that surface — a defect in it is a defect with no second layer behind it. |
| Gating templates through the caller-facing statement guard | One code path for every statement; no separate confinement rule to maintain. | Lost on authorship: it gates the operator's own reviewed text, and a whitelisted query exists to express a read the guard refuses, so the mechanism would admit only statements nobody needed it for. |
| Letting templates bypass the enforcement layers as well as the guard | Maximum expressiveness; the reviewer's judgment is the single control. | Lost on what the review can see: the row and column guarantees would then depend on a reviewer noticing every rule that applies to every table, per template, forever. |
| Confining by shape rather than by identifier | Permits table functions in reviewed forms; more metrics expressible. | Lost on enforceability: what the enforcement layers filter is relations, so a shape allowance that resolves to a path escapes them regardless of how the shape was written. |
| Checking identifiers per request | Catches a manifest mutated after startup; no startup cost. | Lost on evaluation timing against a caller-independent predicate: nothing in the check varies with the request, so it pays per call for an answer fixed at declaration. |

## Criteria

1. **Who authored the statement.** **This criterion decided.** The caller-facing guard's whole
   purpose is to constrain text from an untrusted author; a template's text has a trusted
   author and a review. Applying the same control to both collapses the distinction the
   template mechanism is built on, so no amount of tuning the guard recovers the case.
2. **Reachability of relations outside the filtered namespace** — whether a declaration can
   name something the enforcement layers never see.
3. **Independence from reviewer vigilance** — whether a guarantee holds when the reviewer
   misses something.
4. **Evaluation timing against what the predicate depends on** — a caller-independent check
   belongs where it runs once.
5. **Expressiveness** — which metrics a confinement forecloses. The chosen option is the most
   restrictive of those that pass the second criterion.

## Consequences

An operator writing a metric writes the statement they mean, and the reviewer reads the text
that will execute, with no rewriting layer between review and behavior. Enforcement stays
uniform: a template reading a table is subject to the same row rules and column masks as any
other read of it, so a policy change reaches templates without touching them.

The cost accepted: a template naming a file path directly would read past the tenant filter,
so the identifier confinement is the entire protection on that surface. There is no defense in
depth behind it — a parsing gap in the plain-identifier check is a direct read of arbitrary
bytes under an operator-authored statement. That is why the rule is stated as a shape
predicate over identifiers rather than a denylist of known-dangerous constructs. The
expressiveness cost is concrete and permanent for stores that support table functions: a
metric requiring one is not expressible as a template and is built as a materialized model
instead.

Startup-time evaluation means a manifest change that introduces a bad identifier stops the
face rather than failing a single call. That is the same fail-closed trade the rest of
template validation carries.

## Revisit triggers

- A store's table-function surface becomes filterable by the enforcement layers, at which
  point the confinement is narrower than it needs to be.
- A metric with real demand proves inexpressible under plain identifiers and materializing it
  as a model is not a viable substitute.
- A manifest mutation path appears that can change templates after startup, breaking the
  premise that one evaluation covers every call.
