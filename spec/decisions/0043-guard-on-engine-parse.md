# 0043 — The statement guard reads the executor's own serialization of a statement

**Status:** accepted 2026-09-18
**Decides:** `read.guard.refusal.single-read-only-statement`

## Context

Caller-written SQL arrives at this face as untrusted text and has one admission point. The
executor is an embedded columnar SQL engine linked into the binary, speaking standard SQL. It
accepts a great deal more than reads: attach, copy, install, load, pragma, set, schema changes
and data modifications are all statements it will happily execute against the process's own
filesystem and network.

Admission therefore has to decide one question — is this text exactly one read-only SELECT —
about the same text the executor will run. Every way of deciding it other than asking the
executor introduces a second parser, and two parsers over one dialect disagree at the edges:
comment syntax, quoting rules, string escapes, statement separators inside literals,
dialect-specific clauses. Each disagreement is a place where the guard reads one statement and
the executor runs another, and the caller chooses which.

The engine's own dialect also moves. Statement forms are added by the engine, not by this
system, so any model of the dialect maintained here is stale by construction the moment the
engine is upgraded — and the failure direction of that staleness is admission, not refusal,
because an unrecognized form falls through a blocklist.

The engine exposes a serialization of a parsed statement, and a query form serializes while
other statement forms do not. That property is what makes read-only-ness decidable from the
representation rather than from a list of forbidden words.

## Decision

The guard walks the abstract syntax tree the engine itself executes, obtained by asking the
engine to serialize the statement. Admitted text parses to exactly one read-only SELECT;
attach, copy, install, load, pragma, set, every schema change, every data modification and a
piggybacked second statement all raise `StatementNotReadOnly`. Read-only-ness is a property of
that representation, not of a maintained blocklist.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Walk the executor's own serialization** *(chosen)* | Guard and executor cannot disagree about what a text means. A form the engine does not serialize as a query is refused with no rule written for it. | Admission depends on the engine exposing a serialization for every statement it accepts, and a form the engine serializes as a query but the guard does not model is admitted by default. |
| A token or regex blocklist over the raw text | No dependency on an engine capability; trivially fast. | Loses on divergence — the blocklist and the executor tokenize differently, and a piggybacked statement hides in a comment or a quoted literal. |
| An independent third-party SQL parser | A real parse tree with no engine dependency. | Loses on the same criterion: it models a dialect the executor does not, and the two drift apart at every engine release. |

## Criteria

1. **Non-divergence** — whether the guard and the executor can disagree about what a text
   means. Both rejected options fail this, and the failure is adversarially reachable rather
   than incidental.
2. **Behavior on an unknown statement form** — whether a form nobody wrote a rule for is
   admitted or refused. A blocklist admits; a parse-based guard refuses.
3. **Cost per request** — what admission adds to a read. One parse the executor would perform
   anyway is the floor; a second independent parse doubles it.
4. **Dependency surface** — what the guard requires of the engine. This is the criterion the
   chosen option loses on.

Non-divergence decides it. A guard that is usually right about text an attacker chooses is not
a guard, and the two rejected options both give the attacker the choice of which parser to
write for.

## Consequences

Upgrading the engine does not silently widen the admitted set for statement kinds, since the
serialization test is not enumerating forms. The guard's own code shrinks to a tree walk with
no vocabulary to maintain.

The accepted cost is a hard dependency on an engine behavior. If the engine stops serializing
a statement form, or begins serializing a form that is not a read, admission changes underneath
this decision with no local edit. The residual risk is specific and bounded: a form the engine
serializes as a query but the guard does not model is admitted by default, so the guard's tree
walk has to be exhaustive over node kinds rather than selective. That exhaustiveness is
unverified against engine versions not yet released.

Reversing this is expensive: reintroducing a text-level check would mean maintaining a second
model of the engine's dialect, which is the position this avoids.

## Revisit triggers

- The engine changes or removes the statement-serialization behavior the guard depends on.
- A statement form is found that serializes as a query and performs a write or a filesystem
  reach, which would mean read-only-ness is not a property of the representation after all.
- Per-request admission cost is measured as material against the read's total latency, which
  is unmeasured for very large statements.
