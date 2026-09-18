# 0108 — Only the pipeline's guest configuration table crosses into a guest, and it crosses once per session

**Status:** accepted 2026-09-18
**Decides:** `connector.import.refusal.config-shape`, `connector.import.refusal.config-unclaimed`

## Context

A connector runs as a sandboxed guest importing three things: outgoing HTTP, logging and
a wall clock. There is no filesystem import and no socket import, and no host import
hands secret bytes to a guest. Configuration is the one inward edge that carries operator
intent, and it is the edge where that isolation is easiest to erode.

The source configuration an operator writes is host vocabulary. It names the pipeline's
tables, its cursor kind, its retry posture, its allowlist, its credential bindings —
concepts the host implements and the guest has no business reading. If the whole
configuration crossed, every host key would become guest surface, its meaning would drift
per connector, and renaming a host concept would break connectors that had come to depend
on it.

Credentials are the sharper edge. A `<key>_from = "env:NAME"` spelling is a name the host
resolves and injects at the moment of a request, bound to one declared host. Resolving
such a reference inside a table forwarded to a guest would hand secret bytes across the
boundary through a path with no attach point and no per-request binding — precisely the
ambient authority the connector model exists to eliminate.

The reverse mistake is quieter. An operator writes a guest configuration table for a
connector that exports no configuration interface. The manifest reads as describing a
configured read; nothing configures anything; the connector does whatever its defaults do
and reports success. Where the artifact is local and the probe holds bytes, that is
answerable ahead of the session.

The forwarded table also folds into the connector's content hash, so what crossed is part
of the artifact's identity and a pipeline forwarding no guest table keeps the artifact
digest verbatim as that hash.

## Decision

The pipeline's guest configuration table is the one part of the source configuration that
crosses inward, serialized as a single JSON object and delivered once per session ahead of
discovery and ahead of any open. Ahead of any I/O the host refuses a guest configuration
value that is not a table, a serialization above its size bound, or any credential or
environment reference appearing anywhere inside it, raising `ConnectorConfigRejected`.
Where the artifact is local and the probe holds bytes, a guest table declared against a
guest that exports no configuration interface raises `ConnectorConfigUnclaimed`.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Forward the guest table alone, refuse shape and references ahead of I/O** *(chosen)* | Host vocabulary stays host-side; no path exists by which material crosses as configuration; a declaration that configures nothing is caught | A remote artifact cannot be probed for its configuration export before download, so the unclaimed check runs for local artifacts alone |
| Forward the whole source configuration | One declaration; a connector can read anything an operator wrote | Lost on boundary integrity: host keys become guest surface and change meaning per connector, so the host can no longer rename or restructure its own vocabulary |
| Resolve references inside the forwarded table | Connectors that want a token get it without a host-side attach | Lost on credential containment: it hands secret bytes to a guest through a path with no attach point, no host binding and no per-request scope, which is the ambient authority the model forbids |
| Accept an unclaimed table silently | No refusal to write; a connector may add the interface later | Lost on manifest honesty: the manifest then describes a read that does not happen, and the operator's configuration is inert with nothing saying so |
| Deliver configuration per call rather than per session | A connector could be reconfigured mid-stream | Lost on cost with no case: it multiplies the crossing and the validation for a value that does not change within a session |

## Criteria

1. **Whether host vocabulary can leak into guest code** — whether the host keeps the
   freedom to change its own configuration surface.
2. **Whether material can cross as configuration** — whether any path exists by which
   secret bytes reach a guest outside a host-bound request. *(decided it)*
3. **Whether an operator can write configuration that does nothing.**
4. **Cost per session.**

Credential containment decided it because it is the one criterion whose failure is not
recoverable by a later change. Vocabulary leakage constrains future host refactors and is
felt as friction; an inert configuration key produces a wrong read and is felt as a bug.
A guest that has held plaintext credential bytes has held them, and every downstream
guarantee about bound, per-request, one-host attachment is void for that session and for
whatever the guest did with the value. That asymmetry is why the reference check runs over
the whole table at any depth, ahead of any I/O, rather than at the point of use.

## Consequences

The guest's view of the world is small and stable: three imports, an empty context, and
one JSON object. A connector author reads a documented shape rather than the host's
internal configuration model, and the host stays free to restructure that model.

The size bound and the ahead-of-I/O ordering make the check cheap and complete — no
partial validation, no first request made under an unvalidated configuration.

Folding the forwarded table into the content hash means the configuration a guest ran
under is part of what identifies the run.

The cost accepted sits on the unclaimed arm. A remote artifact cannot be probed for its
configuration export before download, so the check runs for local artifacts alone. A
pipeline pointing at a remote connector with an inert guest table is exactly the case the
refusal exists for and exactly the case it does not cover, and coverage therefore depends
on how an operator happens to distribute connectors rather than on anything the
declaration states.

A connector that genuinely needs host-resolved material also has no route to it through
configuration. It declares a host and an environment name and takes the value attached to
a request, which is more declaration for the author and is the shape the containment
requires.

## Revisit triggers

- Remote artifact distribution gains a cheap metadata probe answering which interfaces a
  guest exports, which closes the unclaimed gap without a download.
- A connector class appears whose configuration legitimately changes within a session,
  which the once-per-session delivery cannot express.
- The size bound is reached by a real connector's configuration, which means the bound is
  measuring something other than the operator intent it was set for.
