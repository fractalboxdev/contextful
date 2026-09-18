# 0139 — A source binding a credential names one non-wildcard host, and an unpermitted guest request fails loudly

**Status:** accepted 2026-09-18
**Decides:** `secret.attach.refusal.unpermitted-request`, `secret.attach.refusal.bound-host`

## Context

The host, not the connector, puts credentials on requests. A guest names a request and
receives a response; no host import hands it credential bytes. What decides whether a
credential attaches is the declaration: a source states a host, and the host writes the bound
credential onto requests permitted by that statement.

A declaration's host entry serves two purposes that pull in different directions. As a
permission it answers "may the guest reach here", and a wildcard is a reasonable thing to
write — a vendor with several subdomains, a CDN, a set of regional endpoints. As a binding it
answers "whose credential is this", and a wildcard destroys the answer. `*.vendor.example`
attached to a bearer token means the token goes to whichever subdomain a guest names, and the
subdomain set is not something anyone enumerated. No check further down can recover which
party the credential was meant for, because the declaration never said.

The guest side adds a second failure. A denied request that returns as an ordinary error
reaches guest code, and guest code routinely swallows errors — a fetch inside a loop, a
best-effort enrichment call, a `try` that returns an empty list. What lands is zero rows,
which read exactly like a vendor with nothing to say. The operator sees a successful run with
an empty table and no reason to suspect the declaration.

## Decision

A source binding any credential carries one non-wildcard host, checked when the declaration
is validated and again when the session opens; a wildcard entry alongside a bound credential
raises `SecretWildcardHost`. A source binding no credential keeps wildcard entries. A request
whose destination the declaration does not cover raises `SecretUnpermittedRequest` and fails
the guest call loudly, surfacing even where guest code discards the error.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Exact host where a credential binds, wildcards where none does, denial surfaces past the guest** *(chosen)* | The party a credential belongs to is stated in the declaration and checkable from it; a denied request cannot be mistaken for an empty vendor | A vendor that genuinely spans subdomains needs one source per host, or a credential-free source |
| Permit a wildcard and attach on a per-request host match | One source covers a multi-subdomain vendor, and the credential still only goes where the pattern allows | Lost on knowability: the intended host is not recoverable from the declaration, so neither a reviewer nor a later check can say which party the material was for |
| Ban wildcards everywhere | One rule, no conditional | Lost on cost with no gain: a source binding no credential has nothing to misattach, and the ban would force per-host sources on reads that carry no material at all |
| Validate at session open alone | One check to implement, and it is the one that actually gates a request | Lost on feedback timing: the operator learns at first run rather than at edit, after a declaration is committed and scheduled |
| Log a denial and return an empty response | The guest's own error handling is untouched, and a run completes | Lost on silence: a guest that swallows the error lands zero rows that read as an empty vendor, and the operator has no signal at all |

## Criteria

1. **Whether the host can know which party a credential belongs to** — whether the binding
   is recoverable from the declaration text. *This is the criterion that decided the wildcard
   refusal.* A wildcard attaches material to unboundedly many hosts and no check downstream
   can recover the intended one, so the question stops being answerable rather than becoming
   harder.
2. **Whether a denial reaches a human** — whether a refused request is distinguishable from
   an empty result. *This is the criterion that decided the guest-visible failure*, and it
   outranks guest-code ergonomics because the alternative failure mode is invisible in both
   the run record and the landed data.
3. **Feedback timing** — whether a mistake is caught at edit or at first run. Decides the
   double check rather than the single one; it is cheap, so it does not have to outrank
   anything.
4. **Declaration economy** — how many sources a vendor takes to express. The chosen option
   loses here and accepts it.

## Consequences

A reviewer reading a declaration can name, per credential, the single party that receives it.
The double check means a wildcard introduced by an edit is refused before the declaration is
committed, and one introduced by any other route is refused when the session opens. A guest
that discards errors still cannot convert a denied request into an empty table.

The accepted cost: a vendor that genuinely spans subdomains needs one source per host, or a
credential-free source. For a vendor with regional endpoints that is several near-identical
declarations, each with its own record entry, and adding a region is an edit rather than a
no-op. A connector author who expected one source to cover a vendor meets this at declaration
time.

A second cost falls on guest code: a call to a destination the declaration does not cover fails the run rather than
degrading. A guest
written to tolerate a partial vendor no longer can, when the partiality comes from the
declaration.

## Revisit triggers

- A declaration grammar gains a way to enumerate a host set explicitly, so a multi-region
  vendor is one source and the party is still recoverable.
- Denied-request refusals are observed dominated by legitimate multi-host vendors rather than
  by declaration mistakes, inverting what the loud failure is protecting.
- A credential form appears that is bound to its destination by the vendor — usable at one
  host regardless of where it is sent — making the declaration's host entry redundant as a
  binding.
