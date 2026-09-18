# 0288 — A snapshot pointer body is wholly a version or it is a fault

**Status:** accepted 2026-09-18
**Decides:** `control.reconcile.refusal.pointer-body`

## Context

A control snapshot store holds one `<store>/manifest@current` pointer whose body is the
applied version as an ASCII integer and nothing else, and one immutable
`<store>/manifest@v{N}.toml` per applied version carrying the canonical document. A poll reads
the pointer and re-parses the named snapshot when its version is strictly greater than the
armed version, so an unchanged pointer costs one small read.

That small read is the whole point of the split. The pointer is read every poll interval — by
default every thirty seconds, per store, forever — while the document is parsed only when the
version moves. Keeping the pointer to a few bytes is what makes the steady state nearly free.

The document the pointer names decides everything the deployment runs. Under a configured
control source the snapshot is the sole source of the pipeline schedule set: local schedules
stay unarmed, and the reconciler owns the entire set the snapshot derives, so an id a newer
snapshot omits leaves the armed set. Arming the wrong version is therefore not a degraded
state; it is a different deployment.

The key is a single object under a prefix a producer writes to. An object accidentally written
to that key — a document, a JSON body, a version with a trailing wrapper, a stray newline
inside a larger payload — is a mundane operational event. What matters is what the reader does
with it. A body carrying a plausible leading integer is indistinguishable from a correct
pointer to any parser that stops at the first non-digit, and the version it yields arms a real,
immutable, parseable document — just not the one anybody applied.

## Decision

A pointer body that is not wholly a version raises `ControlPointerMalformed` rather than
reading a plausible integer out of its leading bytes. An unreadable pointer falls under the
fail-static rule: the armed set already in place keeps running, with a diagnostic logged. The
manifest stays out of the pointer object; the pointer names a version and the version names an
immutable document.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **The body is wholly a version, or it is a fault** *(chosen)* | A pointer either names the version its producer wrote or names nothing. No body can be read as a version by accident. | A producer writing a trailing wrapper or an object body breaks arming until the pointer is rewritten. |
| Parse a leading integer out of a longer body | Tolerant of a trailing newline, a wrapper, or a producer that changed its serialization. | A document accidentally written to the pointer key arms an arbitrary version, and the arming succeeds — the named document exists, parses and is applied. The deployment then runs a configuration nobody chose, silently. |
| Hold the manifest in the pointer object itself | One object, one read, no indirection and no version-skew between pointer and document. | A poll then has no cheap version check and pays a full parse every tick, per store, forever. The steady-state cost of the control plane becomes proportional to document size. |
| Accept a structured body — an object with a `version` field | Extensible: the pointer can carry a content hash or a signature later. | A structured pointer is parsed, and a parser is exactly what turns a malformed body into a plausible value. It also gives up the cheap read the split exists for. |
| Tolerate surrounding whitespace only | Forgiving of the most common producer slip with no ambiguity introduced. | The rule stops being stateable as one sentence, and each tolerance added is a judgement about which accidents are benign — a judgement made by the reader, about bytes the producer did not intend to write. |

## Criteria

1. **Whether a partially-read pointer can arm a version nobody applied** — the failure this
   rule exists to prevent. Leading-integer parsing fails it; structured parsing re-opens it.
2. **Cost of the steady-state poll** — what an unchanged pointer costs, per store, per
   interval. Holding the manifest inline fails this, and so does the structured body.
3. **Statability of the rule** — whether a producer can implement the contract from one
   sentence with no list of tolerated accidents.
4. **Direction of the failure** — whether a malformed pointer stops arming or arms something
   wrong. Fail-static keeps the last-known-good set running either way, so refusing costs
   continuity nothing.
5. **Producer tolerance** — whether a small serialization slip is survivable. This is the
   criterion the chosen option loses on.

Whether a partially-read pointer can arm a version nobody applied decides it, and the deciding
observation is that a plausible leading integer is indistinguishable from a correct pointer.
There is no signal available at read time to separate them, so the only defense is at the
grammar: a body that is wholly a version admits no interpretation. Fail-static is what makes
the trade cheap — refusing the pointer does not stop the deployment, it stops the change.

## Consequences

The pointer read has one outcome per byte string: a version, or a fault. The steady-state cost
of the control plane stays at one small read per store per interval, and the version check
remains a comparison rather than a parse.

The accepted cost falls on producers. Any producer whose serialization emits a trailing
wrapper, a JSON body, or a framing byte breaks arming for that store until the pointer is
rewritten, and the break is total rather than partial — no version advances past it. The
diagnostic names the fault, and the last-known-good armed set continues meanwhile, so the cost
is a stalled rollout rather than an outage.

Reversing this toward tolerance is cheap in code and impossible to bound: each accepted
accident is a new way for a wrong version to arm, and the accidents are discovered one
incident at a time.

## Revisit triggers

- The pointer needs to carry a content hash or a producer signature, which forces a structured
  body and re-opens how a malformed one is told apart from a valid one.
- The control transport reaches off the loopback, where the pointer's integrity is already
  established by producer-side signing over `(version, content-hash)` and the grammar is no
  longer the only defense.
- Producers are observed breaking arming repeatedly on serialization slips, which would price
  the tolerance against the incident rate.
