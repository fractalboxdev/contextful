# 0243 — Only a link established by a verified provider method authorizes

**Status:** accepted 2026-09-18
**Decides:** `visibility.reach.refusal.unverified-link-in-the-join`

## Context

Resolution starts from a subject that admission already settled and has to reach source
principals: the account identifiers that a source's own grant rows name. The workspace
knows a subject; a source knows an account. `access_identity_links` maps one onto the
other, carrying `source_principal`, `subject`, `method` and `confidence`.

How a link came to exist varies enormously in evidentiary weight. A directory
provisioning run out of the identity provider establishes that the provider asserts this
account belongs to this person, which is the same assertion the provider makes when it
issues the credential the reader arrives with. A link inferred from matching display
names establishes that two strings are similar. A link an operator typed into a console
establishes that an operator believes something. These are comparable in an informal sense and not
comparable at all in the sense that matters, because they are not evidence of the same
kind of thing.

The asymmetry of being wrong settles the question. A missing link denies: a subject with
no resolved principal in a source reads none of that source rather than all of it, so the
gap lands as an absence the person notices and an operator fixes. A wrong link
authorizes: the subject inherits everything the mismatched account reaches, silently,
across the whole source, for as long as the link stands. Two people sharing a common
name, a personal account beside a corporate one, a contractor and an employee with the
same initials — each is an ordinary occurrence in a directory of any size.

Profile endpoints are a further trap. A source's profile endpoint returns an email
address for the account making the call, which looks like exactly the evidence a link
needs. Where single sign-on is unenforced on that source, the field is the account
holder's own to edit, which makes the link a self-assertion by the party who benefits
from it.

## Decision

The reachable-set join consumes links whose `method` is `scim_email` or `oidc_sub`. A
link recorded as `operator_asserted` appears in the diagnostic trace, marked as
conferring nothing, and adds no resource. A link whose method sits outside the verified
set reaching the reachable-set computation raises `VisibilityUnverifiedLink`, naming the
method and the source. `confidence` is recorded beside the method and enters no
threshold. Links are written by directory provisioning out of the identity provider, and
never derived from a value a source's profile endpoint returned during a request.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Verified provider methods authorize; every other method is diagnostic** *(chosen)* | Every edge in the reachable set traces to the same authority that vouches for the reader's own identity, and an operator's staging work is visible without being effective. | A subject whose source account was never provisioned reads nothing from that source until directory provisioning catches up, and an operator who has correctly identified a mapping by hand watches it confer nothing. |
| Display-name or email-similarity matching | Coverage on day one, with no provisioning dependency, across sources that were never wired to the directory. | Loses on the failure asymmetry: a false match is a silent, source-wide disclosure to a person who never asked for it, and the sources with the messiest account naming are the ones the heuristic runs on hardest. |
| A confidence threshold admitting high-scoring heuristic links | A tunable dial between coverage and risk, and a number an operator can defend. | Loses on the same criterion: a score is a statement about string similarity, not evidence that the provider verified anything, so raising the threshold reduces how often the disclosure happens without changing what it is. |
| Operator-asserted links authorize | An operator can unblock a person immediately without waiting on provisioning. | Loses on the failure asymmetry: the operator's belief is not checked by anything, and an account attached to the wrong subject grants exactly as widely as a heuristic mismatch does. |
| Resolve the link per request from the source's profile endpoint | Always current, no mirrored link table, no provisioning lag. | Loses on trust in the evidence: where single sign-on is unenforced the field is editable by the account holder, so the reader supplies the input that decides what the reader reaches. |

## Criteria

1. **What a wrong link costs in each direction** — a missing link fails as a denial, a
   wrong link fails as a disclosure. *This criterion decides.* Coverage lost to a missing
   link is visible, bounded to one subject and one source, and fixed by the provisioning
   system that should have produced it; a wrong link is invisible, source-wide, and
   discovered only if someone happens to notice content they should not reach.
2. **Whether the method is evidence from the party that vouches for identity** — the same
   authority chain the credential itself rests on.
3. **Coverage on a source the directory has not reached** — how much of the corpus
   answers before provisioning is complete.
4. **Diagnosability** — whether an operator can see the link they intended, and see that
   it confers nothing.

## Consequences

The reachable set has one provenance story: every principal in it came from the identity
provider. That makes the explain output straightforward to read and makes a disclosure
investigation short, because there is no heuristic layer to reconstruct.

The accepted cost is that provisioning becomes a hard dependency on reading anything from
a governed source. A newly hired person, a source added before its directory integration,
a contractor outside the provider's scope — each reads nothing from the affected source
until the provisioning path catches up, and no operator action shortens that wait.
Operator-staged mappings are visible in the trace precisely so the gap is diagnosable
while it confers nothing.

Reversing toward heuristic links is expensive because the disclosures it would create are
undetectable after the fact: nothing in the store distinguishes a resource read through a
correct link from one read through a mismatched one.

## Revisit triggers

- A provider method appears that verifies account ownership without being `scim_email` or
  `oidc_sub`, at which point the verified set is a list to extend rather than a rule to
  change.
- A source is found whose profile endpoint is bound to enforced single sign-on, making
  its returned identity a provider assertion rather than a user-editable field.
- Provisioning lag on the deployments in use is measured and lands high enough that the
  denial cost dominates, which would argue for a time-boxed, audited, operator-asserted
  path rather than for heuristics.
