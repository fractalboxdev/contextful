# 0203 — A checkpoint declaring an expected audience refuses a credential whose audience differs and one carrying none

**Status:** accepted 2026-09-18
**Decides:** `authority.verify.refusal.audience-mismatch`

## Context

An audience names the deployment a credential was minted for. A credential minted by
exchange carries the deployment audience; a credential minted without an explicit one
carries the audience the project's persisted issuance policy names. Verification checks
signature, expiry, possession and grants — all of which a credential minted for a different
store passes cleanly, because nothing in them is deployment-specific.

That is the exposure the audience exists to close. Stores share an issuer more often than is
comfortable: a staging deployment and a production deployment provisioned from one template,
two tenants served by one organization's issuance, a replica set and the primary that feeds
it. A credential taken from the first admits at the second unless something compares
deployments.

The hard part is the credential carrying no audience at all. There is a reading under which
an absent audience means "not restricted to any deployment" and should therefore match
everywhere, and it is the reading that makes development convenient. It is also the reading
under which the audience check is optional from the presenter's side: a credential minted
with no audience is replayable against every store that checks audiences, which defeats the
check for exactly the credentials most likely to have been minted carelessly.

The local shape is real, though. A developer running against a project on their own machine
has no deployment audience, and demanding one turns the first useful command into a
provisioning exercise.

## Decision

A checkpoint declaring an expected audience raises `AudienceMismatch` for a credential whose
audience differs and for one carrying no audience at all, so a credential minted for one
store is not replayable against another. An undeclared expected audience performs no such
check, which is the local development shape.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Declare-to-check; a declaring checkpoint refuses both a mismatch and an absence** *(chosen)* | A deployment that names itself is unreachable by any credential not minted for it, and the local shape survives by declaring nothing | A single credential cannot serve two deployments, so an operator working across stores holds one credential per store |
| Treat an absent audience as matching any expected audience | Unbound credentials work everywhere; no coordination between mint and deployment | Lost on replay: the property is opt-out from the presenter's side, so a credential minted without an audience is replayable against every store, including the ones that thought they were checking |
| Refuse everywhere, including where no audience is declared | One rule, no configuration, nothing optional | Lost on the local shape: a checkpoint with no deployment identity has nothing to compare against, so the only satisfiable behavior is an invented audience every local project must be provisioned with |
| Check the audience at the mint alone | No per-request comparison; the credential is correct when made | Lost outright: the check has to run at the party being replayed against, and the mint has no knowledge of where a credential is later presented |
| Match an audience by prefix or pattern, so one credential covers a family | An operator's credential spans staging and production with one mint | Lost on the same replay criterion, narrowed: a pattern is chosen once and a generous pattern is indistinguishable at admission from a deliberate one, which is the property the check exists to remove |

## Criteria

1. **Whether a credential is replayable across deployments** — whether possession at one
   store reaches another. *This criterion decides.*
2. **Whether the property is opt-out from the presenting side** — whether a credential can
   decline to be bound.
3. **Viability of the local development shape** without provisioning.
4. **Operator convenience across several stores.**

Criterion 1 decides, and criterion 2 is what eliminates the otherwise-attractive option.
Treating an absent audience as a match keeps every deployment's configuration intact while
handing the presenter the ability to bypass it — and the credentials that carry no audience
are disproportionately the ones minted outside a configured path, which is the population
least likely to be well controlled. Criterion 3 is answered by the check being conditional on
the checkpoint declaring an audience, which is a decision made by the deployment rather than
by anything a caller presents.

## Consequences

A deployment that names itself is closed to credentials minted elsewhere under the same
issuer, so sharing an issuer across staging, production and tenants stops implying shared
reach. Local work needs no audience at all, and adopting one later is a checkpoint-side
declaration plus a persisted default at the mint.

The cost accepted is per-store credentials. An operator working across several deployments
holds one credential per deployment and cannot carry a single one between them, which is
more mints and more to keep track of. A tool automating work across stores handles a set
rather than a value.

Reversing toward absent-matches-any is a small change and reopens replay for the entire
population of unbound credentials at once, including ones already in circulation.

## Revisit triggers

- A legitimate caller shape needs one credential across two deployments — a cross-store
  administrative tool would be the case — and per-store issuance proves unworkable for it.
- A deployment identity becomes available at every checkpoint including local ones, which
  would remove the reason the check is conditional.
- Credentials minted with no audience become rare enough that treating absence as a mismatch
  never fires, which would argue for refusing absence unconditionally instead.
