# 0046 — Template argument binding refuses ahead of execution rather than coercing

**Status:** accepted 2026-09-18
**Decides:** `read.guard.refusal.template-binding`

## Context

A template declares an identifier, one SQL statement, and positional typed parameters written
`name:type` over integer, float, string, timestamp and boolean. A caller supplies values for
those parameters and nothing else; the SQL itself is operator-authored and the caller never
touches it.

Values describing the caller reach a statement through parameter slots, so a value carrying
SQL syntax stays inert — a parameter slot cannot become syntax. That closes the injection
question and opens a different one: what happens when the value is well-formed but wrong.

A wrong value binds cleanly. A string where an integer was declared, coerced, becomes a
number; a timestamp parsed loosely becomes an instant nobody intended; a missing argument
filled with a default becomes a query about a different subject. Every one of those produces a
well-formed query that executes successfully and returns rows. The caller receives a
plausible answer to a question it did not ask, and there is nothing in the response
distinguishing that from a correct answer — a ranked read's counts, a row ceiling's truncation
flag and an empty result are all honest reports about the query that ran.

The empty case is the sharpest. Returning zero rows is a success on this face, and it is the
answer a caller uses to say the store holds nothing on a subject. A mistyped argument that
silently matched nothing is therefore indistinguishable from a true absence.

Positional parameters add a second failure of the same kind: a placeholder count that does not
match the declared parameter count binds the right values to the wrong slots, which is a query
about a permuted question.

## Decision

Argument binding is strict. A missing, unknown or type-mismatched value raises
`TemplateArgumentRejected` ahead of any execution, with no silent coercion. Placeholders cover
exactly the declared parameters, and an identifier colliding with a built-in tool prefix is
refused at manifest validation and at face startup alike.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse a missing, unknown or mistyped argument before any SQL runs** *(chosen)* | A caller's own error is distinguishable from a store that holds nothing. A permuted binding is impossible. | A caller that relied on loose typing writes explicit conversions, and every template edit re-runs both checks. |
| Coerce a value to the declared type | Callers send whatever their language produced and it works. | Loses on silent coercion — a mistyped bound produces a well-formed query answering the wrong question, and the answer is indistinguishable from a right one. |
| Defer the placeholder-count check to execution | One less startup check; a mismatch surfaces as an engine error. | Loses on the same criterion: a positional mismatch binds the right values to the wrong slots, which the engine accepts and answers. |

## Criteria

1. **Silent coercion** — whether a wrong argument can reach the engine as a plausible value.
   Both rejected options fail this.
2. **Distinguishability of caller error from store state** — whether a caller can tell its own
   mistake from an empty store. This is the same property viewed from the caller's side, and it
   is the one that makes the first criterion consequential rather than merely tidy.
3. **Caller convenience** — how much work a caller does to call a template. This is the
   criterion the chosen option loses on.
4. **Namespace integrity** — whether a template identifier can shadow a built-in tool. Refusing
   the collision at both check sites settles this ahead of any listing.

Silent coercion decides it. Every other failure mode on this face announces itself — a refused
statement, a truncation flag, a missing relation — and a coerced argument is the one that does
not. A surface whose wrong answers look exactly like its right answers cannot be built on.

## Consequences

A caller's failures are its own and arrive with a name, so a client library can distinguish a
malformed call from a legitimate empty result without heuristics. Template identifiers cannot
shadow a built-in tool, so a listing means what it says.

The accepted cost falls on callers in loosely typed languages, which now convert explicitly at
the call site — every numeric argument arriving as a string is a refusal rather than a working
call. Template authoring pays too: every edit re-runs the manifest check and the startup check,
so a broken edit stops the face rather than degrading one tool.

Reversing this is cheap mechanically and costly in practice: once callers depend on strict
binding to distinguish their errors, introducing coercion re-hides exactly the class of bug
this exists to surface.

## Revisit triggers

- A declared parameter type is found to have no unambiguous wire representation in a supported
  client language, making strict binding unusable rather than merely strict.
- Refusal rates on well-formed calls are observed high enough that the check is rejecting
  intent rather than error.
- Parameters become named rather than positional, which removes the placeholder-permutation
  failure and changes what the count check is protecting against.
