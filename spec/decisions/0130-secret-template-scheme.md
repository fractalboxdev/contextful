# 0130 — A value template accepts one placeholder scheme, and template hydration treats the process environment as a miss

**Status:** accepted 2026-09-18
**Decides:** `secret.reference.refusal.foreign-placeholder`, `secret.reference.refusal.malformed-placeholder`, `secret.reference.refusal.environment-is-a-miss`, `secret.resolve.refusal.unresolved-name`

## Context

Most outbound credentials are not sent whole. They sit inside a larger value: `Bearer <token>`
in an `Authorization` header, a token embedded in a query parameter, a key concatenated with
an account identifier. A declaration therefore carries a template — literal text with
placeholders — and the host hydrates it at the moment it builds the request.

A declaration is committed to a repository and read by everyone with repository access. That
is what makes the template grammar a security boundary rather than a convenience: whatever a
template can express, any author with commit access can express, and whatever it resolves is
resolved with the daemon's own authority at request time.

Two placeholder spellings ask to be supported and should not be. `${env://NAME}` and bare
`${NAME}` both mean "read the process environment". The daemon's environment holds every
credential every configured source uses, plus whatever the deployment's runtime put there.
A template author writing `${env://OTHER_CONNECTOR_TOKEN}` into their own header value reads
another connector's credential and sends it to their own vendor. The resolver chain already
contains a process-environment adapter for whole-value bindings, so this is not a matter of
adding a capability — it is a matter of whether template hydration consults it.

Malformed placeholders are the third case. An unclosed, empty or nested placeholder has no
resolution, and the lenient answer — pass the literal text through — sends `${secret://vendor`
to the vendor as the value of an authorization header. The text that leaves is at best a
failed request and at worst a fragment of a reference in someone else's logs.

The environment skip has a development cost. Pasting a token into a shell variable is how a
person tries a connector for the first time, and closing that path off entirely makes the
first five minutes with the engine worse.

## Decision

`${secret://<name>}` is the sole placeholder a value template accepts. `${env://NAME}` and a
bare `${NAME}` raise `SecretForeignPlaceholder` at parse, naming the value and the offending
span. An unclosed, empty or nested placeholder raises `SecretMalformedTemplate` while the
declaration is being read, ahead of any text leaving the process. Template hydration treats
the process-environment provider as a miss and continues down the chain. A reference no
assembled adapter answers raises `SecretUnresolvedReference`, failing the source at preflight
where the binding is known statically and failing the call otherwise.
`CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES=1` re-admits the environment provider to template
hydration and warns once per process.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **One placeholder scheme; environment is a miss during template hydration; malformed text refuses at parse** *(chosen)* | A declaration author reaches exactly the credentials the operator bound to logical names, and no arbitrary text leaves as a header value. | Local development loses the paste-a-token path unless the opt-in flag re-opens it, and the re-opened path is indistinguishable from ordinary policy in the run's provider attribution. |
| Inline material in the declaration | No indirection at all; the value is where it is used. | Lost on safe-to-read: the committed file becomes a secret, and every reader of the repository holds the credential. |
| A storage path as the reference | Points directly at the backing store; no name mapping to maintain. | Lost on backend independence: switching stores orphans every binding in every declaration, so a migration is a repository-wide edit rather than an adapter re-point. |
| Permitting `${env://NAME}` in templates | The paste-a-token path works everywhere with no flag and no warning. | Lost on author reach: any declaration author reads any variable the daemon holds, including another connector's credential, and sends it outbound. |
| Silently passing a malformed placeholder through | No parse-time failure; a typo degrades to a failed vendor call. | Lost on containment: the literal text leaves the process for the vendor, carrying part of a reference into someone else's logs. |
| Refusing malformed text at hydration rather than at parse | One code path; hydration already inspects the template. | Lost on when the failure is discoverable: parse happens while the declaration is read, before any request exists, which is strictly earlier for the same guarantee. |

## Criteria

1. **Whether a committed declaration is safe to read.** Everyone with repository access
   reads it.
2. **Author reach.** Whether a declaration author's placeholder can name material the
   operator did not bind for that declaration.
3. **Backend independence.** Whether changing credential stores edits declarations.
4. **Containment of text that leaves the process.**
5. **First-run ergonomics for a developer.**

Criterion 2 decides the environment skip, which is the substantive part of this record.
Criterion 1 rules out inline material and criterion 3 rules out storage paths, but those are
uncontested; the live question is whether template hydration may read the environment, and
author reach is what answers it. Criterion 5 is what the decision spends, and the opt-in flag
is the price paid to spend less of it.

## Consequences

A declaration file commits, diffs and reviews as ordinary text, and its author's reach is the
set of logical names the operator bound. Moving between credential stores re-points an
adapter and edits no declaration. A malformed template fails while the file is being read,
with the value and the span named, so the error points at a line rather than at a failed
vendor call.

The cost accepted: local development loses the paste-a-token path unless
`CONTEXTFUL_SECRETS_ALLOW_ENV_TEMPLATES=1` re-opens it. That flag's re-opened path is
indistinguishable from ordinary policy in the run's provider attribution — the audit records
which adapter answered, not whether the answer was permitted — which is why the flag warns
once per process naming the variable and the access it re-opens. A deployment that sets the
flag and ignores the warning is back to unbounded author reach with no signal in the audit.

Every template author now needs the operator to have bound a name, so a first-time connector
trial involves two people rather than one unless the flag is set.

## Revisit triggers

- Provider attribution gains a policy dimension, so a re-opened environment read is
  distinguishable in the audit and the warning stops being the only signal.
- The opt-in flag is observed set in non-development deployments, which means the ergonomic
  cost is being paid in the wrong place and the development path needs a different answer.
- A credential backend appears whose logical names are themselves scoped per declaration,
  which would make author reach a property of the backend rather than of the grammar.
