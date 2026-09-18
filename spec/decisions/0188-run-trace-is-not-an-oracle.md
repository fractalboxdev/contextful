# 0188 — A refused run trace echoes no pipeline, so a held run identifier answers nothing

**Status:** accepted 2026-09-18
**Decides:** `authority.grant.refusal.run-trace`

## Context

Tracing a run is authorized against the pipeline that produced it. The caller presents a
run identifier; the engine resolves that identifier through the run record to a pipeline
and checks the credential's coverage of that pipeline. The resource being protected is
therefore not the one the caller named.

Run identifiers circulate widely. They appear in run records, in logs, in a step's
output, in anything a pipeline lands that stamps its provenance, and in messages passed
between agents. Possession of one is not evidence of anything — it is the kind of value
that leaks by design, since its whole purpose is to let a later reader join back to the
run that produced a row.

That combination creates a specific hazard. If the refusal names the pipeline the engine
resolved, then anyone holding a run identifier can learn which pipeline produced it,
without holding any grant at all. The refusal becomes a lookup service: present an
identifier, receive the mapping. The information withheld from the trace is handed over
by the message that withholds it.

This is the case where a refusal that names the resource is wrong, and it differs from
the ordinary one in exactly one respect — the caller did not supply the resource being
named.

## Decision

Tracing a run gates on the pipeline resolved from the run record and raises
`GrantRunTraceDenied` without echoing which pipeline that was. A held run identifier does
not become an oracle for its owner.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Gate on the resolved pipeline; refuse without naming it** *(chosen)* | A run identifier discloses nothing a grant would not already disclose. The refusal is the same regardless of what the identifier resolved to. | A caller holding a legitimate run identifier and no coverage gets a refusal naming nothing actionable, and has to go to whoever issued its credential to find out what to ask for. |
| Name the resolved pipeline in the refusal, as the describe path does | Diagnosable; uniform with every other coverage refusal. | Loses on disclosure: the caller did not supply the pipeline, so naming it hands over the run-to-pipeline mapping to anyone who can obtain a run identifier — which is a low bar. |
| Refuse every trace unless the credential covers all pipelines | No resolution happens before the check, so nothing can leak through it. | Loses on usability: a holder of a narrow, legitimate grant could not trace runs of the pipeline it does hold, which is the main reason to trace at all. |
| Treat run identifiers as unguessable and skip the gate | No refusal, no disclosure question, no work. | Loses on what possession means: identifiers appear in records, logs and landed rows, so holding one is evidence of having read something, not of being authorized to read more. |
| Return an empty trace instead of a refusal | Discloses nothing. | Loses on truthfulness, for the same reason an empty run history is rejected elsewhere: an empty trace is a claim that the run did nothing. |

## Criteria

1. **Disclosure beyond what the caller supplied** — whether the response tells the
   caller something it did not already hold. *This criterion decides.* Every other
   coverage refusal in the system names its resource, and can afford to, because the
   caller named it first; here the engine performs a resolution the caller could not,
   and echoing the result gives away exactly the work the gate exists to protect.
2. **Diagnosability** — whether a caller can act on the refusal alone. Given up.
3. **Uniformity with the describe refusal** — one shape of coverage error. Given up.
4. **Usability under a narrow grant** — whether legitimate tracing still works.

## Consequences

A run identifier becomes safe to circulate: it can appear in outputs, logs and messages
between agents without carrying authority or answering questions about the topology of
what runs.

Support cost rises. A refusal that names nothing is a refusal an operator cannot resolve
from the message, so diagnosis moves to whoever holds the issuing side and can see what
the credential covers. That is the accepted cost, and it is paid by the party with the
least context.

The two coverage refusals now differ deliberately, and the distinction is subtle enough
to be re-collapsed by accident. Anyone adding a surface that resolves an identifier into
a different resource before checking coverage has to make this same call again.

Reversing toward naming the pipeline is cheap to implement and expensive in effect: it
would retroactively turn every run identifier already in circulation into a query.

## Revisit triggers

- Run identifiers become unguessable *and* unshared — never landed with rows, never
  logged — which would make possession evidence of authorization and remove the hazard.
- A per-caller diagnostic channel exists that carries what the refusal cannot, letting a
  holder self-serve the diagnosis without the response disclosing it.
- The run-to-pipeline mapping becomes public within a project by some other decision, at
  which point withholding it here protects nothing.
