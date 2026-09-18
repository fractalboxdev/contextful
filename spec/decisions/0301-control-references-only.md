# 0301 — The configuration document holds credential references and never material

**Status:** accepted 2026-09-18
**Decides:** `control.edit.refusal.secret-material-in-the-document`

## Context

Configuration for a store moves through one editable document. That document is a CRDT
persisted per store under conditional replacement, it is applied into an immutable
version, and every daemon and every replica of the deployment reads the materialized
result. It is also, by construction, the thing an operator types into — and the fields an
operator types into include a connector's credentials, since a connector that pulls from
an upstream cannot start without one.

The document therefore has a wide readership and a durable one. Edit-time replication
carries every intermediate state of it between browser sessions; the applied version is
immutable and stays readable at the key that named it, since retention of superseded
versions is operator policy rather than something the owner collects. A value that enters
the document once is present in a replicated log, in a claimed version nobody deletes, and
in the working set of every consumer that materializes the snapshot.

A credential has the opposite shape. Its blast radius is the set of processes that can
read it, its useful lifetime is short, and rotating it is supposed to be one write in one
place. Those two shapes cannot share a container.

The secrets contract already defines a reference scheme and a backend that resolves it at
the moment a connector runs. The question is only whether the operator surface is allowed
to bypass that scheme when it is more convenient to type a value than to place one.

## Decision

The configuration document holds credential references. A credential value typed into a
configuration field raises `SecretMaterialInDocument`. The connector-configuration
workflow collects the value into the secret backend and writes the reference alone into
the document, so the document names where material lives and never carries it. The backend
holds material and resolves a reference at run time.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **References only; the value goes to the backend and never enters the document** *(chosen)* | One place holds material, so rotation is one write and revocation is complete. The document stays safe to replicate, version, retain and read on every consumer. | An operator cannot complete a credential entry while the secret backend is unreachable, so standing up a store depends on a second service being live. |
| Store the value in the document, encrypted | No second service in the entry path; the document remains self-contained. | Lost on readership: every consumer that materializes the snapshot needs decryption material, which turns each of them into a secret holder and the document into a secret store with the weakest consumer setting its strength. |
| Store the value and strip it during materialization | Consumers never see material; the operator path stays simple. | Lost on readership again, one layer earlier: the edit-time document still replicates plaintext between sessions and the claimed version still retains it. Stripping at the last hop protects the last hop only. |
| Accept a value and write it through to the backend transparently, keeping the field | Least friction for an operator, who types into one place. | Lost on rotation and on audit: the field then reads back as if it held the value, and nothing distinguishes a reference an operator placed from one the surface synthesized. Two spellings of one fact drift. |

## Criteria

1. **Readership of the container** — how many processes and how many retained artifacts can
   read whatever the document carries. **This criterion decided.** The document is
   replicated at edit time, immutable once applied, retained at operator discretion and
   consumed by every daemon; nothing with that readership can be a secret container, and
   every alternative failed here first.
2. **Rotation cost** — the number of writes needed to replace a compromised credential. One,
   under a reference; one per retained version otherwise.
3. **Revocation completeness** — whether replacing material anywhere leaves a readable copy
   somewhere. A retained version is exactly such a copy.
4. **Operator friction** — the number of steps and live dependencies a credential entry
   needs. This criterion points the other way and is accepted as the cost.

## Consequences

Rotation becomes a backend operation with no configuration change and no apply, and an
applied version can be shared, diffed and retained without treating it as sensitive. The
audit record of a configuration change is readable in full. A consumer links nothing that
can decrypt.

The cost accepted: the credential-entry path carries a hard dependency on a reachable
secret backend. An operator standing up a deployment cannot finish a connector's
configuration during a backend outage, and the dry-run action that exercises a connector's
health check inherits that dependency. The surface reports the backend as unreachable
rather than offering a path that would work without it.

Reversing this is expensive in one direction only: admitting material later would require
re-deciding retention and consumer trust for every version already claimed.

## Revisit triggers

- A deployment profile exists with no secret backend available to it at all, so the
  dependency is unsatisfiable rather than merely inconvenient.
- Credential entry failures attributable to backend unavailability become a common
  operator-visible fault rather than a rare one.
- The reference scheme grows a form that cannot be resolved at run time by the consumer
  that needs it, forcing material closer to the consumer.
