# 0212 — An embedding application holds no project-wide credential and mints per signed-in reader per request

**Status:** accepted 2026-09-18
**Decides:** `authority.exchange.invariant.per-reader-token`

## Context

An application embeds the read path so its own signed-in users can ask questions of a
project's data. The application has an identity provider that verifies those users and a
backend that can hold key custody. The engine has no user directory of its own and
consumes no directory-synchronization feed; tenancy travels per credential, and verifying
who a principal acts for is the minting backend's obligation.

A credential's grants are a ceiling. Everything downstream narrows within them: the
registered relation is compiled from the resolved grants, the tenant equality reads the
subject's account member, and the row predicate joins against the subject relation. None
of that can recover a distinction the credential never carried. If one credential covers
every reader, every reader's ceiling is the same ceiling, and the enforcement stack is
enforcing it correctly.

That is what makes the conventional arrangement — one application credential, plus
application-side filtering by the signed-in user — a disclosure waiting for a bug. The
filter that distinguishes readers sits outside the enforced relation, in the
application's own code, on the same side of the boundary as the request handling. A
mistake there is not a failed check; it is rows the engine was asked for and correctly
returned.

The other pressure is theft exposure. A credential the application holds is long-lived by
construction, sits in a backend process, and covers every reader. A credential minted for
one reader covers one reader and lapses in minutes, and its confirmation claim binds it to
a key pair its holder possesses.

## Decision

An application embedding the read path holds no project-wide credential. Per request it
trades its signed-in reader's verified assertion for that reader's short-lived,
least-privilege credential and reads as them. The root capability stays in the embedding
application's backend key custody; a browser, an agent client and anything delivered to a
tenant hold a per-request minted credential and nothing more. An exchanged credential is
held against the reader it was minted for and is never reused for a second reader.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Per-reader, per-request mint from the reader's verified assertion** *(chosen)* | The reader's identity is inside the enforced ceiling; a filter bug cannot widen it; theft yields one reader's minutes-long, key-bound credential | Every embedding application needs an identity provider that verifies its readers, and one mint per request sits on the read path |
| One application credential plus application-side filtering by user | No provider requirement; one credential to manage; no per-request mint | Loses on where the ceiling can leak: the filter sits outside the enforced relation, so a bug in it is a disclosure rather than a failed check |
| Long-lived per-reader credentials, cached by the application | Per-reader ceilings with one mint per session rather than per request | Loses on theft exposure: a stolen credential is useful for its whole lifetime, and the short lifetime plus possession binding is the defense being relied on |
| Application forwards the raw provider assertion on every read | No mint at all; the engine sees the provider's own claim | Loses on read-path independence: every read then depends on provider key material and provider availability, which the exchange exists to keep off that path |

## Criteria

1. **Where a scope ceiling can leak.** *(decided it)* The other criteria are cost and
   exposure, both of which can be reduced by other means. This one is structural: an
   application credential's scope silently becomes every one of its users' ceiling, and
   no downstream check recovers the distinction, because every downstream check is
   working correctly on the ceiling it was given.
2. **Theft exposure of the credential in hand.** A per-request credential is narrow,
   short, and bound to a key its holder possesses.
3. **Whether provider availability reaches the read path.** Met by minting once and
   verifying locally thereafter.
4. **Deployment prerequisites for the embedding application.** Conceded, and the whole
   cost of this decision.

## Consequences

The enforced relation and the audit record both carry the actual reader, so an auditor
asking what a person saw reads one record rather than reconciling engine logs against
application logs. An application-side bug can fail to make a request, but it cannot make a
request wider than its reader's grants. Tenancy rests on a bound predicate over a subject
member the mint stamped, rather than on the application remembering to filter.

The cost accepted is a hard prerequisite: every embedding application needs an identity
provider that verifies its readers, and an application whose users are not provisioned
from such a directory cannot embed at all. That excludes applications with their own
homegrown user tables and no assertion to present. The per-request mint also sits on the
read path, adding signing work to each request, though not a network call once the
verifying material is injected.

Moving to an application credential later would be a one-line change in the application
and an unauditable widening of every reader's ceiling, which is why the prerequisite is
stated as a condition of embedding rather than a recommendation.

## Revisit triggers

- Per-request mint cost is measured as a material share of read latency at realistic
  request rates.
- A credible embedding shape appears where readers are verified by something that is not
  an assertion-issuing provider, and no adapter can produce one.
- The enforced relation gains a way to carry a per-request reader identity that the
  credential does not, making the ceiling argument no longer structural.
