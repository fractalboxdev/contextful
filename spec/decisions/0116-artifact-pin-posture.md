# 0116 — A remote connector artifact refuses without a content pin, and a local one refuses under either pin switch

**Status:** accepted 2026-09-18
**Decides:** `connector.package.refusal.remote-unpinned`, `connector.package.refusal.insecure-artifact`, `connector.package.refusal.digest-mismatch`, `connector.package.refusal.local-unpinned`, `connector.package.refusal.posture-undeclared`

## Context

A connector name resolves to bytes in one of four forms: in-tree, a project-local path, an
HTTPS URL, or an OCI reference. Every control above those bytes — the capability allowlist,
the declared environment names, the scope expectation, the guest's own filters over what it
lands — is a statement about a particular program. If the bytes are not pinned, none of
those statements binds anything. The allowlist governs whatever code arrived this morning.

A version tag does not close that. A tag is a name a publisher can repoint, and repointing
it is how ordinary publishing works. The property needed here is that the bytes the engine
runs are the bytes someone reviewed, which is a property of a content digest and of nothing
else. Plain HTTP fails it a second way: the transport carrying the artifact is itself
substitutable, so a pin fetched over cleartext is a pin an intermediary chose.

Local artifacts sit differently. During authoring, every rebuild changes the digest, and a
requirement that the digest be current turns a build-run-edit loop into a pin-build-run-edit
loop. But a project that has finished authoring and now runs a local artifact in production
wants exactly the remote property. Two parties have standing to decide: the operator, for
the whole store, and the connector author, for their own artifact.

That gives a manifest two postures — adopted, carrying a digest; or a template, carrying the
requirement plus a placeholder the pin verb writes into — and a manifest carrying neither is
a third thing nobody authored deliberately.

## Decision

A remote artifact carrying no 64-hex content pin raises `ConnectorRemoteUnpinned` at parse,
so it fails the validation verb and the gate alike rather than at first load. A plain-HTTP
artifact reference raises `ConnectorInsecureArtifact` outright. The host re-hashes the
resolved bytes and raises `ConnectorDigestMismatch` before they reach the engine. Two
switches close the unpinned local load and compose by disjunction — a store-wide policy key
and a per-connector flag — and with either set, an unpinned local artifact raises
`ConnectorLocalUnpinned` at build, the refusal carrying the digest of the bytes it found, so
pinning is a paste. A manifest that neither carries a digest nor declares both template
markers raises `ConnectorPostureUndeclared` at merge, and a rebuild sweep judges every
adopted value.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Pin remote always; pin local under either switch** *(chosen)* | Every control above the bytes binds particular bytes in production, while the authoring loop stays a build-and-run. | A store that never sets the policy key runs whatever bytes sit on disk, and the two-switch disjunction means the effective posture is not readable from one file. |
| Require a pin on a local artifact always | One rule; no switch; no posture to declare. | Lost on the development loop: every rebuild changes the digest, so the author pins on each iteration or turns the check off, and the second is what happens. |
| Trust a version tag on a remote artifact | Familiar publishing ergonomics; upgrades arrive without an edit. | Lost on what the controls bind: a tag is repointable, so the reviewed program and the running program are the same only by the publisher's continued good behavior. |
| Verify the digest at first load rather than at parse | Fewer checks to run; the failure still happens. | Lost on when it is discoverable: the validation verb and the gate both pass, and the refusal arrives at run time on an operator's machine instead of in review. |
| Treat a declared requirement with no digest as a parse error | One fewer state; a manifest is well formed or it is not. | Lost on expressiveness: it makes the template posture inexpressible, since a template is by definition a well-formed manifest with a placeholder where the pin goes. |

## Criteria

1. **Whether the allowlist and the guest's filters bind any particular bytes.** *This
   criterion decided it.* Every other control in the connector contract is stated about a
   program. Unpinned bytes make all of them statements about nothing in particular, so this
   criterion is upstream of the rest rather than beside them.
2. **Usability of the build-run-edit loop.** Whether an author can iterate without pinning
   each time.
3. **Where a refusal is discoverable.** Parse and build against first load.
4. **Whether each posture a manifest can hold is one somebody chose.** Whether an
   undeclared third state is reachable.

## Consequences

A reviewed connector is the running connector. The refusal for an unpinned local artifact
carries the digest of the bytes it found, so adopting a pin is a copy and a paste rather
than a separate command. The rebuild sweep judges every adopted value, so a manifest whose
artifact drifted from its digest is caught in bulk rather than one load at a time.

The accepted cost has two parts. A store that never sets the policy key, and whose
connector authors never set the flag, runs whatever local bytes sit on disk — the protection
is opt-in on that path, and an operator who assumes otherwise is wrong. And because the two
switches compose by disjunction, the effective posture for a given connector is not readable
from any single file: an operator reads the store policy and the manifest together to know
what the loader will do.

## Revisit triggers

- Deployments are found running local artifacts in production with neither switch set,
  which would argue for inverting the default rather than keeping the requirement opt-in.
- The unreadable-from-one-file posture causes a real misjudgment — an operator believing a
  connector is pinned when it is not — which would argue for surfacing the effective posture
  in the validation output or the run record.
- A signing and transparency layer above the content pin arrives for community-distributed
  connectors, which changes what a digest alone is being asked to carry.
