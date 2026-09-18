# 0149 — Whether a derive pipeline carries a shared-quota grant is keyed on its task

**Status:** accepted 2026-09-18
**Decides:** `derive.select.refusal.unmetered-grant`, `derive.select.refusal.metered-client`

## Context

The run path meters egress. A pipeline that opens sockets to third parties attaches a
shared-quota grant to its client, every request it makes enters the run's request ledger, and
the budget is enforced across pipelines rather than per pipeline. A pipeline that reaches
nothing carries no grant, and declaring one would claim a share of a budget it never spends.

Everywhere else in the system, the answer to "does this pipeline reach vendors" is a property
of the connector: the feed reader does, the local directory walker does not, and a list of
connector names keyed by that property answers correctly for all of them.

Derive breaks that assumption, and it is the only connector that does. One connector name
covers two tasks with opposite answers. A `link_preview` pipeline exists to follow addresses a
third party wrote; its whole operation is egress and it belongs in the ledger. A `transcribe`
pipeline runs an operator-declared binary against a local file; nothing it does opens a socket
it can observe, and a preprocess step that fetches reports that traffic as the step's own.

A name-keyed list therefore has no correct entry for `derive`. Listing it nags a transcription
pipeline for a grant it cannot spend and cannot justify; omitting it leaves a link pipeline
making unbounded requests with nothing in the ledger.

## Decision

Whether a derive pipeline reaches vendors is read off `task` in the source specification, with
no connector build consulted. A `transcribe` pipeline declaring a shared-quota grant raises
`DeriveUnmeteredGrant`. A `link_preview` pipeline opening a socket outside the mediated client
raises `DeriveMeteredClient`; every request it makes enters the run's request ledger.

Manifest validation parses the `task` key to answer the question, at the point where the
manifest is checked and before any engine is built.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Key the answer on `task`, read from the source specification** *(chosen)* | Both tasks get the correct answer; the check runs at manifest validation with no engine construction | Manifest validation parses one configuration key to answer a question it would otherwise answer from a constant |
| A list of connector names that reach vendors | One constant, no parsing, uniform with every other connector | Lost on correctness for a connector that is both: the single entry for `derive` either nags a transcribe pipeline for a grant it cannot carry, or stays quiet about a link pipeline running unmetered |
| Split `derive` into two connector names | Restores the name-keyed list and its constant lookup | Lost on duplication: the scan, the anti-join, the unit loop, the attempt accounting and the landing rules are shared, so two names would front one implementation and every future modality would add a third |
| Ask the built engine whether it reaches vendors | Authoritative — the adapter knows | Lost on timing: the engine is built after the manifest is accepted, so a misdeclared grant would surface at the first scheduled tick rather than while the operator is watching |
| Meter every derive pipeline unconditionally | One rule, no branch, fails safe | Lost on truth in the ledger: a transcription run would consume a shared egress budget it never spends, and the budget stops describing egress |

## Criteria

1. **Whether one answer can be correct for a connector that is both** — whether the keying
   property distinguishes the two tasks at all. **This criterion decided it, alone.** Every
   other candidate property — connector name, a constant, a safe default — yields one answer
   for two opposite behaviors, so it is wrong for one of them by construction, and no
   implementation cost can compensate for an answer that cannot be right.
2. **When a misdeclaration surfaces** — at manifest validation with the operator watching, or
   at the first scheduled tick.
3. **Whether the ledger keeps describing egress** — whether a metered figure still corresponds
   to requests actually made.
4. **Duplication across tasks** — how much of the shared scan and landing path a candidate
   forces to be written twice.

## Consequences

Manifest validation reaches into `[pipeline.source.config]` for `task` before any connector is
built, which makes the metering check dependent on a key a connector owns. Every other
connector answers this question without opening its configuration. That is the cost accepted:
one special case in validation, in exchange for two correct answers.

What gets easier: a new derive modality declares its egress posture by naming a task, and the
metering answer follows without touching a list elsewhere.

What is now expensive to reverse: run records already carry the per-task answer, so a later
move to a connector-name keying would relabel historical runs' egress posture.

## Revisit triggers

- A third derive task arrives whose egress posture is not a function of the task name — for
  instance one whose engine may be local or remote depending on its binding.
- Metering moves from a manifest-declared grant to an observed property of the client, making
  the declaration redundant.
