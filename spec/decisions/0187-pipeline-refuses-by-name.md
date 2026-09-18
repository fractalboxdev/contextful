# 0187 — An ungranted pipeline is refused by name, never answered with an empty history

**Status:** accepted 2026-09-18
**Decides:** `authority.grant.refusal.ungranted-pipeline`

## Context

A pipeline identifier is a read resource in its own right. Listing pipelines, describing
one and tracing its runs each require a read grant covering that identifier, and a grant
over the tables a pipeline lands does not confer it — the two are separate resources
that happen to be related by what the pipeline writes.

That means a caller regularly asks about a pipeline its credential does not cover. What
the engine hands back in that case is a design decision, and the tempting answer is the
quiet one: return an empty run history and let the caller draw its own conclusions.

The reason that is wrong is that an empty history is not an absence of information. It
is a positive claim: this pipeline exists and nothing ran. A caller that acts on the
claim — an agent deciding whether to fire a run, an operator deciding whether an
ingestion is stalled — acts on a fact the engine did not verify and does not believe.
Two very different states, "you may not see this" and "there is nothing to see", come
back as identical bytes.

The listing surface has the same question at a different grain. A list filtered to
covered identifiers is honest; a list that is empty because the credential covers none is
indistinguishable from a project that declares no pipelines at all.

## Decision

Describing a pipeline no grant covers raises `GrantPipelineNotCovered` and names the
identifier. Listing filters down to the covered identifiers, and raises the same
identifier when the credential covers none. A project that declares no pipelines answers
with an empty list. A denial and a fact are therefore never the same response.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Refuse by name; filter the listing; empty list only when the project declares none** *(chosen)* | A caller can always distinguish "denied" from "nothing there". Diagnosis is immediate and names the resource. | The response distinguishes a project that declares no pipelines from one whose pipelines the caller cannot see — a caller learns that *something* exists without learning what. |
| Answer an empty run history | Discloses nothing at all about coverage. | Loses on truthfulness: an empty history is a claim that nothing ran, and callers act on it. A denial presented as a fact is worse than a denial. |
| A generic refusal that names no identifier | Uniform response, nothing echoed. | Loses on diagnosability while buying no confidentiality here: the caller supplied the identifier, so naming it back reveals nothing the caller did not already hold. |
| Refuse only on describe; return an empty list for an uncovered listing | Simpler listing path. | Loses on truthfulness at the listing grain, which is the surface an agent uses to decide what exists at all. |
| Grant pipeline access implicitly from a grant over the tables it lands | No separate resource; fewer refusals. | Loses on what the resource is: a run history discloses schedules, failures and timing that the landed rows do not, so table coverage is not evidence of authorization over the pipeline. |

## Criteria

1. **A denial is never indistinguishable from a fact** — whether a refusal can be read
   by a caller as a substantive answer. *This criterion decides.* The other properties
   affect how pleasant the surface is to use; this one decides whether a caller can
   trust anything it returns, and a surface that lies by omission poisons every
   conclusion drawn downstream.
2. **Diagnosability** — whether the caller can tell what was refused and act on it.
3. **Existence confidentiality** — what the response discloses beyond what the caller
   supplied. Partly given up, and bounded by the fact that the caller names the resource.
4. **Uniformity of the refusal across surfaces** — describe, list and trace behaving
   alike where they can.

## Consequences

An agent can reason about pipelines it cannot see: it gets a refusal naming an
identifier it already held, which is a request to widen its grant rather than a silent
dead end. Operators debugging an ingestion stop confusing a permission problem with a
scheduling problem.

The accepted cost is a coarse existence signal. A caller holding no pipeline coverage
learns from the refusal that the project declares at least one pipeline, because a
project declaring none answers with an empty list instead. That is a deliberate exchange:
one bit about the project, in return for never presenting a denial as data.

Reversing this is expensive on the caller side rather than the engine side — callers
that learn to treat an empty history as a fact would have to unlearn it, and the code
that did so would be wrong without being obviously wrong.

## Revisit triggers

- The one-bit existence signal matters to a deployment — a multi-tenant face where
  knowing that a project runs anything at all is itself sensitive — which would put the
  confidentiality criterion into contention.
- Pipeline coverage becomes derivable from table coverage by an explicit, granted rule,
  removing the separate resource and with it most of these refusals.
- A caller surface appears where identifiers are not caller-supplied, which is the
  condition that keeps the confidentiality cost bounded here.
