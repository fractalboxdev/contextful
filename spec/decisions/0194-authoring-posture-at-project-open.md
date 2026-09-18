# 0194 — Every project-load site supplies an authoring posture, and the per-request posture withholds the ambient principal

**Status:** accepted 2026-09-18
**Decides:** `authority.issue.interface.authoring-posture`

## Context

A process that opens a project may hold an ambient credential — one verified at startup,
belonging to whoever launched the process. Two deployment shapes hold one and want opposite
things from it.

A single-person tool holds an ambient credential that is the answer. The command line, the
desktop client and a local agent each run as one person, every write is theirs, and
requiring a credential per write would be ceremony over a fact already established. Writes
arriving with nothing attached are correctly authored by the ambient principal.

A served face holds an ambient credential that is a hazard. It belongs to the operator who
started the daemon, and the writes flowing through it belong to callers. A write that
arrives unaccompanied on such a face is not the operator's; it is a write whose author is
unknown. Authoring it by the process stamps the operator's verified principal onto rows they
never authored, and that value is exactly what per-principal placement policy, identity links
and the audit record read as identity.

The two shapes cannot be told apart from inside the project-load call. Both are one process
holding one credential and opening one tree. The difference is in what the process does next,
which the load site knows and the loaded project does not.

## Decision

The authoring posture is an argument every project-load site supplies. Under a `session`
posture the project verifies one ambient credential and authors every write through it by
that principal. Under a `per_request` posture the ambient principal is withheld entirely, so
a write arriving unaccompanied is authored by nobody rather than by the process. Neither is
a default; a load site states which it is.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A posture argument at every load site, with no default** *(chosen)* | The one party that knows how the project will be used is the party that answers, and answering is unavoidable | Every load site carries an argument, including the many where the answer is obvious, and a served face that passes the wrong value mis-authors silently |
| Always session — the ambient principal authors unaccompanied writes | Nothing to configure; single-user tools work with no ceremony | Lost on attribution: a served face stamps the operator onto every caller's rows, and the values are indistinguishable afterwards from rows that principal really authored |
| Always per-request — no ambient principal ever authors | Fails closed; no face can mis-attribute | Lost on the local shape: a single-person tool lands every one of its own rows with no author, which turns off per-principal policy and empties the audit record for the deployment that needs neither of the protections this buys |
| Infer the posture from the deployment shape — a listening socket means per-request | No argument to pass; the common cases resolve themselves | Lost on silence. The inference is a guess made inside the load, it is wrong for an embedded face and for a daemon serving one person, and being wrong produces no error — only rows attributed to the wrong principal |
| Default to per-request, allow session to be opted into | Fails closed while keeping the option | Lost on the same criterion as the always-per-request option, narrowed: the tool that needs `session` is the one least likely to read the argument's documentation, so the failure mode is authorless local rows, quietly |

## Criteria

1. **Whether a wrong answer is observable** — whether the failure announces itself or shows
   up as rows carrying the wrong principal. *This criterion decides.*
2. **Attribution exactness on a face serving many principals.**
3. **Whether the deployment that needs no protection pays for it** — the single-person case.
4. **Call-site cost** — how many sites carry an argument whose answer is obvious.

Criterion 1 outranks attribution itself. Both wrong answers here are attribution failures,
but they differ in how they present: the always-per-request failure leaves an empty column
an operator notices, while the always-session failure fills the column with a plausible,
verified, wrong principal that reads as correct at every consumer. The inference option loses
on the same point — a guess that cannot fail loudly is worse than an argument that cannot be
omitted. Criterion 4 is the live cost and it is paid once per load site.

## Consequences

A served face cannot accidentally lend its operator's identity to a caller, and a local tool
keeps its ergonomics without a per-write credential. The posture is visible at the call site,
so reading how a deployment authors its rows means reading one argument rather than tracing
what credential the process happened to hold.

The cost accepted is that the argument exists everywhere and can be passed wrongly. A face
that passes `session` authors every caller's write by the operator, and nothing in the engine
detects it — the principal is genuinely verified, and the rows are well formed. The guard
against that is the choice being explicit rather than the engine catching it.

Reversing toward an inferred posture is cheap in code and expensive in what it gives up: the
rows written under a wrong inference are correct-looking and unrecoverable.

## Revisit triggers

- A third shape appears that is neither one ambient principal nor none — a face holding a
  small fixed set of principals would be one — which the two-value posture cannot express.
- A load site is found passing `session` on a face serving several principals, which would
  argue for a structural constraint rather than an argument.
- The engine gains a way to detect a mismatch between the declared posture and the observed
  caller population, which would make an inferred default safe.
