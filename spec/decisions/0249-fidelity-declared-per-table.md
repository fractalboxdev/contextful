# 0249 — Fidelity is declared per table, and a federated table is neither registered for reading nor cached across subjects

**Status:** accepted 2026-09-18
**Decides:** `visibility.declare-fidelity.refusal.federated-table-registered`, `visibility.declare-fidelity.refusal.federated-cache-shared`

## Context

Sources differ in how exactly their audience can be reproduced. One exposes per-principal
access lists at the same grain it enforces, and the mirror joins them exactly. Another
exposes only which workspace or container an object belongs to, so the reproduced audience
is a container-shaped approximation of the real one. A third decides access at request
time with no list to read, and the only faithful thing to do is ask it, per reader, with
that reader's own delegated credential. A fourth has no safe path at all.

A deployment spans all four at once. The guarantee it can make about an answer is
therefore not one guarantee: it is a property of whichever tables that particular answer
touched. Stating it once at deployment level means stating the weakest case for
everything or the strongest case for nothing, and the first is false about most answers
while the second is false about some.

Federation raises a separate problem that has nothing to do with precision. A federated
leg runs under one reader's own delegated credential, which means its result is
authorized for that reader and for nobody else. Two ordinary system behaviors will leak
it if they are allowed to. Registering a federated table on a read face puts rows under a
relation that the enforced view governs — except that the store holds no rows for it, so
what such a registration actually produces is a table that either answers nothing
confusingly or, if something populates it, answers from one reader's credential to
whoever queries next. Caching a federated result under a key describing the query rather
than the reader does the same thing more directly.

The reader-facing side of this is smaller than it looks. A reader who is told an answer
was drawn at container grain, or drawn live from a source under their own access, knows
what they have. Being told nothing is what leaves them assuming exactness.

## Decision

Each table declares one of four levels — `mirrored`, `coarse`, `federated`, `excluded` —
and a `coarse` binding puts the source's own grain name into the response envelope, so a
reader is told the scope of the approximation rather than the fact of it. Rows are served
out of the store at `mirrored` and at `coarse`. A `federated` resource is swept into the
resource table holding zero grants and its content rows enter no store table; registering
a `federated` table on a read face raises `VisibilityFederatedRegistered`, and its
observable effect is the resource's absence, with the trace printing the class so the
absence reads as the level rather than as a missing grant. Retaining a federated result
under a key that omits the subject raises `VisibilityFederatedCacheShared`. An answer
drawn from both the mirror and a live query marks the leg on each citation.

## Options considered

| Option | What it buys | What it costs |
| --- | --- | --- |
| **A level per table, with its grain in the envelope, and federated tables unregistered and never cached across subjects** *(chosen)* | A reader learns how exact each part of an answer is, and the one result authorized by a single reader's own credential has no route to another reader. | Every answer carries qualification a reader has to absorb, two sources answering one question can disagree about their precision, and federated legs pay full cost per reader with no shared cache. |
| One deployment-level permission-aware claim | A single sentence in the product, one guarantee to explain, and no per-table vocabulary. | Loses on falsifiability: the weakest source silently sets the real guarantee for everything, so the claim is either untrue about the tables it overstates or useless about the ones it understates, and nothing in the system detects which. |
| Exclude every source that is not exact | The strongest possible claim, uniformly true, with no approximation anywhere. | Loses on corpus coverage: it removes most of the corpus to obtain a guarantee the reader can simply be told, and the removed sources are frequently the ones holding the answers people ask for. |
| Register federated tables and let the enforced view govern them | Uniform surface: every table is a relation, and federation is an implementation detail beneath it. | Loses on where a cross-reader disclosure can enter: the store holds no rows for such a table, so anything populating the relation is holding one reader's credentialed result where another reader's query will find it. |
| Cache federated results by query shape | Federation performs like the mirror, and a repeated question costs one upstream call. | Loses on the same criterion, directly: the result was authorized by one reader's own delegated credential and belongs to no other, so a subject-free key is a disclosure by construction. |
| Declare the level per source rather than per table | Fewer declarations, and precision is usually a property of the source anyway. | Loses on fit: one source commonly exposes both an exactly-mirrorable object type and a container-only one, so a per-source claim overstates the second or understates the first. |

## Criteria

1. **Whether a reader can tell how exact their answer is** — falsifiability of the claim
   attached to a result. *This criterion decides.* A precision claim that is not
   per-result is a claim about the deployment's best or worst table rather than about the
   answer in hand, and a reader acting on it is reasoning from a guarantee the answer does
   not carry.
2. **Where a cross-reader disclosure can enter** — the registration surface and the cache
   key. This criterion is what makes the two federated refusals absolute rather than
   advisory.
3. **Corpus coverage** — how much of what an organization holds can be answered from.
4. **Reader load** — how much qualification an answer asks someone to absorb.

## Consequences

An answer states the grain of every approximation inside it, names the federated sources
it consulted, and names the ones that failed or timed out, so a reader learns which part
of the question went unanswered rather than receiving a confident partial answer.

The accepted cost is threefold. Readers absorb qualification on ordinary answers. Two
sources answering one question can disagree about their own precision, and reconciling
that is left to the reader. Federated legs pay full cost per reader on every request, with
no shared cache and no contribution to ranking over the store, so a corpus that is mostly
federated is slow and thin in exactly the way that invites someone to mirror it anyway.

Reversing toward a shared federated cache is expensive because the disclosures it creates
are invisible: the second reader's result is well-formed, plausible, and authorized by
somebody else's credential.

## Revisit triggers

- The federated leg's per-reader latency is measured and lands high enough that readers
  route around federated sources rather than waiting.
- A source appears that returns a reader-independent decision set cheaply, which would
  make a subject-free cache safe for that source rather than by construction unsafe.
- Reader comprehension of the `coarse` grain disclosure is observed to be low, which would
  argue about how the qualification is presented rather than about whether it is carried.
