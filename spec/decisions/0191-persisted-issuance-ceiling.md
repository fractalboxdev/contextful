# 0191 — A project persists one issuance ceiling of at most 24 h, in version control, and every mint path obeys it

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.limit.issuance-ceiling`

## Context

Lifetime is the main bound on a credential that has escaped. Derivation is offline, so a
stolen parent mints children without anyone observing it. Withdrawal reaches a checkpoint
through a denylist and a scoped epoch, which a checkpoint reads from a record it refreshes
on its own schedule — so revocation is real but not instant, and it depends on someone
noticing. Expiry depends on nobody noticing anything.

There is more than one way to mint. A command-line mint, a mint endpoint behind an
`admin` grant, and the exchange that trades a verified external assertion for a scoped
credential all produce credentials, on different hosts, run by different people, with
different defaults. A bound that lives in one of those paths bounds one of them.

There is also more than one copy of a project. A project tree is cloned — onto a
developer's machine, into a replica, into a build. A credential minted from any clone
verifies at any checkpoint holding the issuer's public key, so a ceiling that differs per
clone is a ceiling that equals its loosest instance.

The audience has the same shape: a credential minted without an explicit audience gets a
default, and a default that differs by host produces credentials that admit in places
nobody intended.

## Decision

A project persists an issuance policy naming the longest lifetime any mint path
produces, at most 24 h, together with the audience stamped on a credential minted
without an explicit one. The policy is tracked in version control, so every clone
enforces one number and a change to it is a reviewable diff.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One persisted policy in the project tree, tracked in version control, hard ceiling of 24 h** *(chosen)* | Every mint path and every clone enforce the same number. Changing it is visible in review, with an author and a reason. | Changing the ceiling is a commit and a deploy, not an operation. The lowering carries a grace window until the last credential under the old value can have lapsed. |
| A per-mint flag with no persisted ceiling | Maximum flexibility at the moment of minting. | Loses on uniformity: the number becomes whatever each operator typed, differs per host and per path, and no clone can be audited for what it permits. |
| An environment variable or process flag | Easy to set per deployment; no file to manage. | Loses on uniformity the same way, and on auditability: the value is invisible in the tree, so review sees nothing and a clone carries no evidence of what it enforced. |
| A compiled-in constant | Impossible to get wrong per deployment. | Loses on operability: deployments legitimately differ below the hard ceiling, lowering has to be recordable with the instant it happened, and a constant supports neither. |
| A policy service consulted at mint time | Central control; instant fleet-wide change. | Loses on the offline property: minting and verification carry no network dependency today, and adding one puts an availability requirement on the one path that must keep working. |

## Criteria

1. **One enforced number per project, across every mint path and every clone** — whether
   the bound is a property of the project or of whoever happened to run the mint. *This
   criterion decides.* A ceiling that varies is not a ceiling; it is the maximum of its
   instances, and nobody can enumerate the instances.
2. **Auditability** — whether the value and its changes are visible where changes are
   reviewed.
3. **Operability of a change** — whether lowering and raising can be performed, recorded
   and reasoned about.
4. **Independence of the mint path from any service** — no network dependency added.

## Consequences

The ceiling becomes reviewable. A change to how long credentials live shows up as a diff
with an author, which is the surface where a proposed loosening gets argued with.

Lowering the ceiling records the previous value and the instant of the lowering, and a
grace guard validates against the recorded value until the last credential minted under
it can have lapsed — otherwise lowering would retroactively invalidate live credentials.
Raising the ceiling is the dangerous direction and prints a reminder to re-validate the
rotation grace.

The accepted cost is latency of change: an urgent tightening is a commit, a review and a
deploy, not a console action. A deployment that needs the ceiling lowered *now* lowers it
at the speed of its release process.

The policy is also readable by anyone with the tree. That is deliberate — the ceiling is
not a secret, and treating it as one would put it back into the places that failed the
uniformity criterion.

## Revisit triggers

- Withdrawal becomes fast and reliable enough — a denylist and epoch a checkpoint reads
  with a bounded staleness that is measured — that lifetime stops being the main bound.
- An urgent tightening is needed and the release-process latency is observed to be the
  binding constraint during an incident.
- Mint paths appear outside the project tree often enough that persisted-ceiling
  enforcement from inside the tree covers a minority of mints, leaving the rotation-grace
  validation at the checkpoint doing most of the work.
