# D29 — A face admits one verified credential and carries authority as a value

**Status:** accepted

## Context

A served face answers many callers through one daemon. Any path that authenticates a position, a shared secret or the process itself writes rows under the wrong principal, and those rows are indistinguishable afterwards from correctly authored ones.

## Decision

Authority enters through exactly one verification and flows downstream as an explicit value, never as ambient process state.

- `authority.issue` takes an authoring posture as an argument at every project-load site, with no default: `session` authors through one ambient credential; `per_request` withholds the ambient principal.
- A served face admits a verified capability credential and nothing else: no perimeter bearer, gateway shared secret or unauthenticated owner path, behind a flag or otherwise. A gateway trades its caller's assertion for a credential.
- `authority.verify` produces an admitted-authority value carrying the normalized subject and narrowed grants; every read surface and row-landing effect takes it as an argument, and its type exposes no fabricating constructor.
- `authority.exchange` mints per signed-in reader per request; an embedding application holds no project-wide credential and reuses no exchanged credential across readers.

## Options

| Option | Lost on | Cost |
| --- | --- | --- |
| One admission path, authority as an argument, per-reader mint *(chosen)* | — | Every load site and effect carries an extra parameter; every embedding application needs an identity provider; one mint per request sits on the read path. |
| A perimeter secret or loopback owner path, opt-in | Attribution | A subjectless, expiryless owner-equivalent path would stand beside the credential. |
| Infer posture from deployment shape | Silence | A wrong guess would mis-attribute rows with no error. |
| Re-read the environment or a task-local at each effect | Attribution | Two callers through one daemon would both pick up the process's principal. |
| One application credential plus application-side filtering | Enforcement boundary | A filter bug would be a disclosure, not a failed check. |

## Consequences

- A served face passing the wrong posture mis-authors silently; the posture argument is the one place to review it.
- Surfaces reaching beneath tables become grant-filtered tools over the registered relation.

## Revisit

- An embedding shape appears where per-request mint latency dominates reads.
- A face needs a caller class that no grant can express.
