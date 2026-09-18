# 0224 — A table the caller holds no grant on reads as a name nothing is bound to

**Status:** accepted 2026-09-18
**Decides:** `enforcement.refuse.refusal.ungranted-table`

## Context

The relation compiler emits one registered relation per granted table, bound to that table's
bare name inside the session. A name the caller holds no grant on therefore resolves to
nothing: there is no relation registered under it, and the session's catalog has no entry to
find. The question is what the engine says about that, and it is a different question from what
it says when a granted table is read outside the caller's scope.

A permission error is the conventional answer and it has a specific property: it confirms
existence. A caller holding one narrow grant can then walk a list of candidate names and sort
them into three piles — exists and is forbidden, does not exist, is mine. Repeated across a
dictionary of plausible table names, that reconstructs the shape of the deployment: which
tenants are present, which products, which pipelines run. None of that was granted, and each
individual probe looks like an ordinary mistake.

The deployment shape is not incidental information. Table names in this system carry pipeline
identity and source identity, and a caller in a multi-tenant store is often another tenant. A
grant is meant to be a window, not a directory listing with most entries greyed out.

An empty result is the other tempting answer and it implies something untrue: that a table
exists and holds nothing. It also collides with the out-of-scope case, which deliberately
refuses rather than returning empty, and would make a boundary and a quiet table
indistinguishable again in the one place that was just closed.

## Decision

A table the caller holds no grant on raises `EnforceUnknownRelation`: it is absent from
listings, absent from descriptions, and reads as a name nothing is bound to. Holding a grant
never becomes a way to enumerate what exists beyond it, and the ungranted case and the
out-of-scope case answer differently for that purpose.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Read as a name nothing is bound to; absent from listings and descriptions** *(chosen)* | Probing names yields nothing about the deployment: every ungranted name answers the same way a nonexistent one does. | An operator debugging a missing grant sees an unknown-relation error rather than a permission error. |
| Return a permission error naming the table | The most diagnosable answer; the caller learns exactly why the read failed. | Loses on enumeration: it confirms existence, so one narrow grant becomes a probe that maps the whole deployment. |
| Return an empty result | Discloses nothing and requires no error handling on the caller's side. | Loses on truthfulness: it implies a table that exists and holds nothing, and re-introduces the ambiguity the out-of-scope refusal exists to remove. |
| Treat it identically to the out-of-scope case | One refusal shape to implement, document and handle. | Loses on what each case may disclose: the out-of-scope refusal names its table safely only because the caller was already granted that table, and an ungranted name has no such cover. |

## Criteria

1. **Whether a grant becomes a way to enumerate what lies beyond it** — the enumeration
   channel. This is the criterion that decided it.
2. **Truthfulness of the answer** — whether the response implies a state that is not the case.
   The empty result fails this.
3. **Consistency with the out-of-scope refusal** — whether the two cases can be told apart by
   an attacker, and whether that distinction leaks. Collapsing them fails this.
4. **Diagnosability for a legitimate operator** — how quickly a missing grant is identified.
   This is the criterion the chosen option loses on.

Enumeration decides it. Diagnosability is a real loss and it is recoverable by other means — an
operator can read the grant set, which states positively what the caller holds. The enumeration
channel is not recoverable: once a permission error confirms existence, every name a caller
cares to try is answered, and no later control takes that back.

## Consequences

The set of tables a caller can learn anything about is exactly the set it was granted, which
makes the grant's description complete rather than approximate. Listings and descriptions agree
with reads — there is no surface where an ungranted name appears in one and vanishes in the
other.

The accepted cost falls on legitimate debugging. An operator whose grant is missing or
misspelled sees an unknown-relation error, which is the same error a genuine typo produces, and
tells the two apart by reading the grant set rather than by reading the error. That is one more
step in a loop that is already annoying, and it will occasionally send someone looking for a
schema problem that is really a permissions problem.

Reversing this is cheap in code and impossible to undo in effect: any deployment that switches
to a permission error has, from that moment, answered every probe a caller chooses to send.

## Revisit triggers

- A deployment appears where table existence is not sensitive — a single-principal store — and
  the diagnostic cost outweighs an enumeration channel with nobody to enumerate to.
- Operators are observed misdiagnosing missing grants as schema faults often enough to cost
  more than the channel is worth.
- A surface is added that must distinguish "not granted" from "not present" for a legitimate
  reason, such as a provisioning path, which would need its own authority rather than this one.
