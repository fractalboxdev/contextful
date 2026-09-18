# 0009 — The CRDT library links into the control profile alone, and other profiles read materialized text

**Status:** accepted 2026-09-18
**Decides:** `topology.package.refusal.crdt-confinement`

## Context

Configuration for a team is edited by more than one person, often at the same time, and
sometimes while one of them is offline. That is the problem a replicated document solves
well: concurrent edits merge without a lock and without a coordinating server holding the
session. The control plane is where that editing happens, and it is the profile that holds
team state and identity already.

Every other profile reads configuration and writes none of it. The daemon reads it at start
and on reload; the read replica reads what it needs to sync and serve. Neither has an
editor, neither has a second writer to reconcile against, and neither has any use for edit
history.

Two costs follow from letting the replicated document travel further than the editor. The
first is footprint: the CRDT library links into profiles whose budgets are set by what they
serve, and it buys them nothing. The second is subtler — a configuration value read
directly from a replicated document has a different consistency model than the same value
read from materialized text. The daemon would then be a second reader reconciling merge
state, and a configuration value would effectively have more than one writer.

## Decision

The CRDT library links into `contextful-control` and no other profile, enforced by the
dependency audit; appearing in the resolved dependency graph of the edge or full profile
raises `ProfileDependencyLeak`, naming the profile and the path that pulled it.
Configuration has one owner: the control profile holds the edit-time collaborative
document and materializes canonical TOML on apply. A daemon or a replica reads that
materialized text and nothing else.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Replicated document in the control profile, materialized text everywhere else** *(chosen)* | Concurrent multi-editor editing where editing happens, and one writer plus one materialization step everywhere else. No profile reconciles a document it does not edit. | An edit is not live until apply runs, so a window exists where the document and the materialized text disagree; the control plane owns closing it. |
| CRDT configuration everywhere, daemons reading the document directly | An edit propagates with no apply step; no materialization to keep honest. | Lost on footprint in the edge and full profiles, and on writer count: configuration acquires two readers with different consistency models, so what a daemon believes and what an editor sees can differ with no step between them. |
| Plain text with file locking as the only format | One format end to end; no library at all; trivially auditable. | Lost on concurrent multi-editor editing, which the team surface requires — a lock serializes editors and fails badly for an offline one. |
| A server-authoritative editing API with operational transforms | Concurrent editing without a replicated document in any profile. | Lost on offline editing and on effort: it puts a stateful coordination service in the path of an engine whose deployments are otherwise air-gappable. |

## Criteria

1. **Writer count** — how many things can change a configuration value and how many
   consistency models it is read under. **This criterion decided.** One editing surface
   plus one materialization step means a daemon never reconciles a replicated document,
   which keeps configuration a single-writer fact throughout the run path.
2. **Footprint in profiles that never edit configuration** — bytes the edge and full
   profiles would pay for an editor they do not have.
3. **Concurrent editing, including offline** — a requirement the team surface carries.
4. **Propagation latency** — how quickly an edit reaches a running daemon. The chosen
   option is the worst here and lost this criterion deliberately.

## Consequences

The audit states confinement as a graph question with a named path when it fails. The
daemon's configuration handling is ordinary file reading with no merge semantics, which
also means a deployment can inspect and diff its effective configuration with ordinary
tooling. The edge and full budgets are unaffected by the editing surface's evolution.

The cost accepted: an edit is not live until apply runs. There is a window in which the
collaborative document and the materialized text disagree, and a reader looking at the
editor sees something the daemon has not adopted. Closing that window — surfacing the
pending state, sequencing apply, reporting what a daemon has actually loaded — is the
control plane's work, and a deployment that skips apply runs on stale configuration with
no complaint from the engine.

Reversing confinement is cheap mechanically and expensive semantically: once a daemon reads
the document, configuration has two consistency models and every read has to say which.

## Revisit triggers

- The apply window causes repeated operational confusion — daemons observed running
  configuration the editors believe was replaced.
- A CRDT implementation small enough to sit inside the edge and full budgets removes the
  footprint objection, and a single-writer discipline can still be stated.
- Configuration acquires a legitimate second writer, making one-writer-plus-materialization
  no longer an accurate description of the system.
