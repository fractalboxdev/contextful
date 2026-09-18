# 0268 — Template arguments bind strictly, and a declaration validates its placeholders and statement count ahead of any caller

**Status:** accepted 2026-09-18
**Decides:** `disclosure.template.refusal.argument-binding`, `disclosure.template.refusal.placeholder-cover`, `disclosure.template.refusal.statement-count`

## Context

A template exists so that a statement a human reviewed is the statement that runs. The caller
submits an identifier and named arguments and writes no statement text of its own. Everything
that makes the mechanism worth having rests on one property: what the reviewer read and what
executes are the same relation, differing only in the values bound into declared, typed
parameters.

Argument handling is where that property is won or lost, because every lenient behavior
available is a way for the caller to change the relation. A missing argument bound to a
default the reviewer did not write selects a different row set than the reviewed predicate
describes. A string coerced into a numeric parameter becomes a value the caller did not send
and cannot predict — a non-numeric string reaching a comparison as zero reads a wholly
different set of rows. An unknown argument accepted and ignored means the caller believes it
constrained the result and the reviewer's predicate ran unconstrained; the caller's
misunderstanding of the surface produces a confidently wrong answer instead of an error.

The declaration itself has the same exposure from the other side. Placeholders that do not
cover exactly the declared parameter list mean either a parameter that binds nowhere — the
caller's argument is accepted and has no effect — or a placeholder with no parameter behind
it, which is an unbound hole in a reviewed statement. A declaration holding more than one
statement is a reviewed relation with a second relation attached to it, and the row ceiling,
the truncation flag and the projected tool schema all describe the first.

When those declaration faults are caught decides who meets them. Validating at first call
means a malformed template ships, passes every deploy check, and fails on a user's request.
Validating at manifest check and again at face startup, fail-closed, means it fails before
the face serves anything.

## Decision

Binding is strict. A missing argument, an unknown argument and a type-mismatched argument each
raise `DisclosureTemplateBindingMismatch` ahead of execution, and no argument is silently
coerced. Placeholders that do not cover exactly the declared parameter list raise
`DisclosureTemplatePlaceholderMismatch`, and a declaration holding more than one statement
raises `DisclosureTemplateMultiStatement`. Templates validate at manifest check and again at
face startup, fail-closed, before any caller reaches them. Every declared parameter projects
into the template's tool schema as required.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Strict binding, declaration validated at manifest check and at startup** *(chosen)* | What the reviewer read is what runs, with the caller able to vary only declared values within declared types. A malformed declaration never serves a request. | A parameter rename is a breaking change for every caller with no compatibility window, and manifest authors meet validation failures at startup rather than at deploy review. |
| Coercing a mistyped argument to the declared type | Tolerant callers; a loosely typed client works without a type layer of its own. | Lost on reviewed-statement fidelity: a string coerced to zero reads a different set of rows than either the caller or the reviewer intended, and the answer comes back shaped like a correct one. |
| Binding a missing argument to a declared default | Shorter calls; a common parameter is stated once in the manifest. | Lost on the same criterion: the row set a caller receives then depends on a value it never sent, and an omission and a deliberate choice become indistinguishable at the call site. |
| Accepting and ignoring an unknown argument | Forward compatibility — an old face tolerates a caller written against a newer manifest. | Lost on the same criterion: it hides a caller's misunderstanding of the surface, and the ignored constraint is exactly the one the caller believed was applied. |
| Validating the declaration at first call rather than at startup | No startup failure mode; a partly broken manifest still serves its working templates. | Lost on failure timing: the malformed template reaches production and fails on a user's request, which is the one moment the mechanism exists to make boring. |
| Allowing several statements per declaration | Multi-step metrics expressible in one reviewable unit. | Lost on what the surrounding guarantees describe: the row ceiling, the truncation flag and the tool schema each name one relation, so the second statement runs outside all three. |

## Criteria

1. **Fidelity of the reviewed statement** — whether a caller can change what the reviewed
   predicate selects. **This criterion decided.** Every rejected binding behavior fails here
   in the same way, and the mechanism has no value left once it fails: a template whose
   effective predicate depends on caller leniency is a free-form statement with extra steps.
2. **Failure timing** — whether a declaration fault is met by an author or by a user.
3. **Diagnosability** — whether a wrong call produces an error or a plausible wrong answer.
   Coercion is the worst case in the whole set, since nothing distinguishes its output.
4. **Caller tolerance** — how much a client must get right to call successfully. The chosen
   option is the strictest available.
5. **Expressiveness of a declaration** — whether one template can carry a multi-step metric.

## Consequences

A caller that succeeds has sent exactly the declared parameters at the declared types, so
every successful call corresponds to a reviewed relation with known holes filled. Errors
arrive before execution, which keeps the audit record free of entries for reads that were
never coherent. The strict declaration checks make the manifest self-consistent: a parameter
list, its placeholders and the projected tool schema cannot drift apart.

The cost accepted: a template parameter rename is a breaking change for every caller, with no
compatibility window. There is no ignore-unknown path to carry old clients across a rename,
so the manifest author coordinates callers or ships a second template under a new identifier.
The second cost lands on authors: validation failures surface at face startup, so a manifest
that passed review and broke afterwards stops the face rather than degrading. That is
fail-closed on purpose and it is still an outage.

Loosening binding later is cheap to implement and expensive in meaning — every call that
previously errored begins returning rows, and no consumer can tell which of its historical
calls would now be answered differently.

## Revisit triggers

- Client-side type fidelity proves impossible on a transport the deployment must serve, so
  correct callers cannot express a declared type at all.
- Parameter renames become frequent enough that the absent compatibility window is the
  dominant cost of changing a manifest.
- A multi-statement metric appears that cannot be decomposed into separate templates without
  the intermediate relation leaving the engine.
