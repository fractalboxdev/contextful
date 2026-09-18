# 0013 — The engine owns reusable contracts and an application owns its domain vocabulary

**Status:** accepted 2026-09-18
**Decides:** `topology.bound-application.refusal.application-scope`, `topology.bound-application.refusal.shared-abstraction`

## Context

The engine is one code base serving more than one application. Each application arrives
with a domain: role names, a rating schema, category meanings, entity aliases, prompts
and column bindings. Every one of those is a statement about a business, not about
ingestion, storage, retrieval or policy.

The pressure to put the first application's vocabulary into the engine is strong and
local. A default is cheap to add, saves that application a configuration file, and
looks like a sensible general case at the moment it is written. Its cost lands on the
second application, which receives a reserved schema it did not ask for, automatic reads
it did not configure, and retrieval heuristics tuned on another domain's exemplars. That
inheritance is silent: nothing in the second deployment says the defaults were shaped by
a different business, and the failure shows up as answers that are subtly wrong rather
than as an error.

The same pressure produces premature abstraction. Two applications name a thing similarly,
or two pipelines emit a record of the same shape, and the resemblance reads as a shared
concept. Shape agreement is weak evidence: two structs with the same three fields can
have opposite invariants about when a field is null, who may write it, and what a
duplicate means. An abstraction built on the resemblance has to be unbuilt when the
second consumer's real invariants arrive.

The analyst console is the pull in the opposite direction. Per-application forks of it
are the obvious answer to divergent domain needs, and they are the wrong one: the
components, the redactor and the render contract are exactly the parts where a divergence
becomes a leak — a fork that drifts on redaction ships an application that discloses what
the others do not.

## Decision

The engine owns reusable ingestion, storage, memory, query and policy contracts and
nothing about any domain. An application names its vocabulary in a per-store lexicon; the
engine's defaults supply structural conventions with no domain meaning attached, and a
category the lexicon leaves undeclared renders neutral. An application's role name, rating
schema or category meaning appearing in engine code raises
`ApplicationVocabularyInEngine`, naming the identifier and the crate. A behavior becomes a
shared engine abstraction once a second application requires the same behavior under the
same invariants; an abstraction proposed on similar names or matching output shapes alone
raises `PrematureAbstraction`, naming the single consumer it generalizes from. The console
is one shared interface over engine capabilities plus application-supplied metadata, and
the operator view derives its pipeline graph from the pipelines a deployment configures,
with application nodes as an overlay.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **Engine holds contracts, application holds its lexicon** *(chosen)* | Every later application starts from a neutral engine and declares what it means | Domain behavior is declared through overlay surfaces rather than received, so each application writes configuration the first one got for free |
| Ship the first application's vocabulary as engine defaults | One application's setup shrinks to nothing; the defaults look like a general case | Lost on inheritance: a later application receives a reserved schema, automatic reads and exemplar-steered heuristics it never chose, and nothing tells it where they came from |
| Per-application forks of the console | Each domain shapes its own interface without negotiating | Lost on ownership: the components, the redactor and the render contract need one owner, and a fork drifts on exactly the parts whose drift is a disclosure |
| A built-in application graph in the operator view | The graph is right on day one for the application it was drawn for | Lost on derivation: a graph the deployment does not produce goes stale silently, where a graph derived from configured pipelines with explicit nodes as an overlay cannot |

## Criteria

1. **Inheritance** — what a second application receives that it did not choose. *(the one
   that decided it)* A default is a decision made on behalf of a party not yet in the room,
   and its failure mode is a wrong answer rather than an error, so no later party can
   detect it. Every other criterion here trades effort now against effort later;
   inheritance trades a known cost now against an undetectable wrong answer later, which
   is why it outranks them.
2. **Ownership of a disclosure surface** — whether the redactor and the render contract
   have exactly one owner.
3. **Evidence for an abstraction** — whether shared invariants, not shared shapes, justify
   generalizing.
4. **Staleness** — whether a description of the deployment can disagree with the
   deployment.

## Consequences

Adding a second application is a configuration exercise rather than an engine change, and
its lexicon is a file its own team reads. Auditing what a deployment means becomes local:
the lexicon states it.

The cost accepted is duplication and friction. An application declares domain behavior it
would otherwise inherit, an undeclared category renders neutral instead of guessing
correctly, and a genuinely shared behavior waits for its second consumer before it is
factored out — so some duplication lives longer than is comfortable, and a maintainer
fixes the same bug in two places in the interval.

The console is now a negotiated surface. A domain need that the shared components cannot
express is a change to the shared components, discussed with every other consumer, rather
than a local fork — slower, and occasionally the reason a domain need goes unmet.

## Revisit triggers

- Two applications independently implement the same behavior under invariants that a
  reader can state identically. That is the second-consumer condition and the abstraction
  is due.
- A neutral render of an undeclared category is measured as producing a worse answer than
  a domain default would, on a deployment whose lexicon is complete.
- The console's shared render contract accumulates per-application branches, at which point
  the single owner is nominal and the fork already happened inside one file.
