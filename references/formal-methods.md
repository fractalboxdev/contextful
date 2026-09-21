# Formal methods

Executable models, proofs and differential testing for the clauses where prose is
least trustworthy: the store commit and lease protocol, journal crash recovery, and
authorization. The proof package covers authorization; these sources bear on which
other protocols repay a model and which pin type fits which clause kind.

## Model checking and lightweight formal methods

### How Amazon Web Services Uses Formal Methods

Newcombe, C., Rath, T., Zhang, F., Munteanu, B., Brooker, M., Deardeuff, M. "How
Amazon Web Services Uses Formal Methods." *CACM* 58(4):66–73, 2015.
<https://cacm.acm.org/research/how-amazon-web-services-uses-formal-methods/>

- **Priority:** must-read
- **Informs:** `store.lease`, `store.merge`, `store.push`, `run.journal`, `assurance.model`
- **Question:** Which lease + CAS + fence bug appears only at depth? TLA+ found
  DynamoDB and S3 bugs at 35-step traces. One model of the lease, the CAS index
  rebase and its retry bound, and the fence is the cheapest way to settle whether a
  paused holder can commit after its successor, and the store refusals pin to its
  invariants.

### Systems Correctness Practices at Amazon Web Services

Brooker, M., Desai, A. "Systems Correctness Practices at Amazon Web Services."
*CACM*, 2025. <https://dl.acm.org/doi/10.1145/3729175>

- **Priority:** must-read
- **Informs:** `assurance.prove`, `assurance.differential-test`, `assurance.test`, `corpus.state`
- **Question:** Which evidence fits which clause kind? The toolkit — P, TLA+,
  property-based testing, deterministic simulation, Dafny and Lean — with when each
  repays its cost, is the menu for choosing a pin type per clause.

### Using Lightweight Formal Methods to Validate a Key-Value Storage Node in Amazon S3

Bornholt, J., Joshi, R., Astrauskas, V., Cully, B., Kragl, B., Markle, S.,
Sauri, K., Schleit, D., Slatton, G., Tasiran, S., Van Geffen, J., Warfield, A.
"Using Lightweight Formal Methods to Validate a Key-Value Storage Node in Amazon S3."
*SOSP*, 2021. <https://dl.acm.org/doi/10.1145/3477132.3483540>

- **Priority:** must-read
- **Informs:** `assurance.differential-test`, `run.journal`, `store.merge`, `store.fold`
- **Question:** How does conformance checking extend from authorization to crash
  recovery and merge? An executable reference model, property-based and
  crash-consistency conformance checks, and model checking of Rust concurrency, run
  per change on a Rust storage node — the closest template for pinning store and
  journal refusals.

### P

Desai, A., Gupta, V., Jackson, E., Qadeer, S., Rajamani, S., Zufferey, D. "P: Safe
Asynchronous Event-Driven Programming." *PLDI*, 2013.
<https://doi.org/10.1145/2491956.2462184>

- **Priority:** optional
- **Informs:** `run.journal`, `surface.dispatch`, `run.backfill`
- **Question:** Can the workflow clauses be state machines checked against the
  implementation? P specifies asynchronous event-driven protocols as communicating
  state machines, a fit for dispatch, suspension and backfill workflows.

### Alloy

Jackson, D. "Alloy: A Language and Tool for Exploring Software Designs." *CACM*
62(9):66–76, 2019. <https://doi.org/10.1145/3338843>

- **Priority:** optional
- **Informs:** `disclosure.mirror`, `disclosure.reach`
- **Question:** Does bounded model finding over relations check the mirror's
  three-way conjunction and group closure better than prose invariants? Small-scope
  counterexamples cover cycles and depth at the closure bound.

## Verification-guided authorization

### Cedar

Cutler, J. W., et al. "Cedar: A New Language for Expressive, Fast, Safe, and
Analyzable Authorization." *OOPSLA* (PACMPL 8), 2024.
<https://dl.acm.org/doi/10.1145/3649835>

Disselkoen, C., et al. "How We Built Cedar: A Verification-Guided Approach." *FSE
Companion*, 2024. <https://arxiv.org/abs/2407.01688>

- **Priority:** must-read
- **Informs:** `assurance.prove`, `assurance.differential-test`, `assurance.scope-claim`, `assurance.recheck`, `authority.verify`
- **Question:** Which targets repay proof and which repay differential testing? Cedar
  is the published form of the assurance contract: a Lean model with proved
  properties, differential random testing tying the Rust engine to it, and a scoped
  claim. The FSE report counts 4 validator soundness bugs found by proof against 21
  bugs found by testing, which argues for committing to few proof targets —
  inclusion and narrowing — and letting testing carry the rest.

### Verus and Kani

Lattuada, A., Hance, T., Cho, C., Brun, M., Subasinghe, I., Zhou, Y., Howell, J.,
Parno, B., Hawblitzel, C. "Verus: Verifying Rust Programs using Linear Ghost Types."
*OOPSLA*, 2023. <https://arxiv.org/abs/2303.05491>

VanHattum, A., Schwartz-Narbonne, D., Chong, N., Sampson, A. "Verifying Dynamic
Trait Objects in Rust." *ICSE-SEIP*, 2022.
<https://dl.acm.org/doi/abs/10.1145/3510457.3513031>

- **Priority:** should-read
- **Informs:** `assurance.scope-claim`, `assurance.prove`
- **Question:** How does the translation gap between the Lean model and the Rust
  code close? Kani harnesses on the decision functions are the cheaper route; Verus
  proofs are the stronger one.

## Differential testing of SQL

### Pivoted Query Synthesis

Rigger, M., Su, Z. "Testing Database Engines via Pivoted Query Synthesis." *OSDI*,
pp. 667–682, 2020. <https://www.usenix.org/conference/osdi20/presentation/rigger>

- **Priority:** optional
- **Informs:** `authority.compose`, `read.guard`, `assurance.differential-test`
- **Question:** What oracle checks that no withheld row influences a result? Queries
  generated to return a known pivot row, run through the enforcement rewrite over
  DuckDB, test the pass-precedes-the-cut ordering directly.
