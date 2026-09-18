# 0025 — A declared encryption key source that the process cannot resolve refuses startup

**Status:** accepted 2026-09-18
**Decides:** `store.encrypt.refusal.key-source-unbound`

## Context

At-rest encryption is a per-project declaration. A project that carries an `[encryption]`
block writes its Parquet through native modular encryption and encrypts every sidecar
beside it under the same key; a project with no such block writes cleartext and claims
nothing. The two states are distinguishable to a reader only by what the block says,
because the bytes on disk are the only other evidence and reading them is not something a
deployment does on a schedule.

The key itself never sits in the project configuration. It resolves from the process
environment through `key_source`, so one image serves several deployments and the key
material stays out of the tree the store publishes. That indirection introduces a state
the declaration alone cannot describe: the block is present, the variable is not.

The consequence of guessing at that state lands unattended. Ingestion runs on a schedule
against a bucket, and the first write after a deployment is typically the first time the
key would be needed. Whatever the process decides at that moment is what every subsequent
row inherits, and nothing in the store records that a decision was made.

An operator reading the project configuration afterwards sees a store that declares
encryption. An auditor reading the same file reaches the same conclusion. Only someone
who opens a Parquet footer and finds no encryption metadata learns otherwise, and by then
the cleartext is already in the bucket, already replicated to whatever consumes it, and
already outside the deployment's control.

## Decision

A `key_source` naming an environment variable the process does not hold refuses startup
and raises `StoreEncryptionKeyUnbound`. The check runs at process start, before any table
is opened and before any row lands. A declared encryption block and a resolvable key are
one state; a declared block with nothing behind it is not a degraded mode of that state
but a configuration error the process declines to run under. A literal key in the
configuration file is not an accepted alternative source, so the value has exactly one
place to come from.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse startup when the named variable is unset** *(chosen)* | The declaration and the bytes agree at every instant the process is alive. The failure is loud, immediate, and lands while a human is still watching the deploy. | A deployment that forgets one variable does not start at all, and the deploy environment joins the surface that has to be validated. |
| Fall back to writing cleartext and log a warning | The deployment survives a missing variable and keeps ingesting. | Lost on failure shape. The store reports itself encrypted and is not, and the only instrument that detects it is reading the bytes. A warning in a log nobody reads is not a signal. |
| Defer the check to the first write that needs the key | No startup cost, and a deployment that never writes never pays for a key it does not use. | Lost on failure shape as well, one step removed. The deploy environment is validated at start; the first write happens unattended, and a refusal there surfaces as a failed run rather than a failed deploy. |
| Accept a literal key and an environment binding under a precedence rule | An operator can pin a key inline for a local run without touching the environment. | Lost on ambiguity. Two sources for one value turns every incident into a question about which one won, and the losing source is usually the one a reader is looking at. |

## Criteria

1. **Failure shape** — whether being wrong produces an error, or produces a store that is
   silently inconsistent with what it declares. *This criterion decided it.* Encryption is
   a claim made to someone outside the deployment, and a claim that fails open is worse
   than no claim: it is believed. Every other criterion here is about convenience during a
   deploy, which is recoverable within minutes; a cleartext object already copied to a
   consumer is not recoverable at all.
2. **Time to detection** — how long a wrong state survives before anyone can observe it. A
   startup refusal is observed in seconds; a cleartext write is observed when someone
   inspects a Parquet footer, which may be never.
3. **Number of sources for one value** — how many places a reader consults to learn what
   key the process holds.
4. **Cost of the check** — round trips and permissions the validation itself requires. An
   environment read costs nothing, which is why this criterion carries little weight here
   and would carry more against a check that had to reach a key-management service.

## Consequences

Startup validation becomes the place a deployment learns its environment is incomplete,
and the unbound key is reported alongside every other unbound binding rather than in
isolation. An operator reading a project configuration can trust the encryption block:
if the process is running, the key resolved.

The deploy environment is now part of the validation surface. A container orchestrator
that drops a variable produces a process that will not start, which reads as an outage
rather than as a misconfiguration until someone opens the log. Rolling a key by changing
the variable requires the new value to be present before the restart, not after.

Reversing this is inexpensive in code and expensive in trust. Once deployments rely on the
refusal, a fallback added later makes every existing store's encryption claim conditional
on which build wrote it, and the claim would have to be re-established per snapshot rather
than per project.

## Revisit triggers

- A key-management path exists whose resolution can legitimately fail transiently at
  startup and succeed moments later, making a hard refusal a liveness problem rather than
  a safety one.
- The catalog records the key version of every snapshot in a form a reader can check
  offline, so a cleartext write becomes detectable without opening a Parquet footer.
- A deployment shape appears where encryption is genuinely optional per table rather than
  per project, which makes a project-wide declaration the wrong unit for this refusal.
