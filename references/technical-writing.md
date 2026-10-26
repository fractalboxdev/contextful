# Technical writing brief for agents

Technical guides give readers enough understanding to trace a mechanism, explain its purpose and recognize its limits. Plain language preserves the technology names, interfaces, data formats and conditions that make the explanation correct. These editorial practices apply to guides and technical explanations; contract clauses follow `corpus.anatomy`, and design rationale follows `corpus.rationale`.

## Choose a structure for the reader's task

| Reader's task | Useful structure | Reading example |
| --- | --- | --- |
| Understand a mechanism | A concrete problem, a small example, successive mechanisms, then remaining limitations | *STARKs, Part I* and the SNARK introduction |
| Compare alternatives | Shared goal, comparison criteria, repeated treatment of benefits and costs, exceptions | *The different types of ZK-EVMs* |
| Evaluate a proposal | Existing mechanism, demonstrated limitation, proposed change, compatibility costs | *Multidimensional gas pricing* |
| Understand a design direction | Goal, governing principle, consequences for each subsystem, transition strategy | *Simplifying the L1* |

A diagram can precede detailed examples when it makes a comparison legible, as in the ZK-EVM taxonomy. Example-first is a useful teaching technique, not a mandatory page template.

## Editorial practices

1. **Introduce the problem that makes a detail necessary.** Explain the failure or constraint before adding its mechanism. A reader encountering credential attachment needs to understand which destination receives the credential and why destination checks matter.
2. **Define terms through their role.** At first use, explain what a checkpoint checks, what a connector runs, or what an audit establishes. Name assumed prerequisites and link their explanation. A link supplements the reasoning needed to understand the current paragraph.
3. **Retain precise technology names.** Keep WebAssembly, Arrow IPC, Biscuit, SQL and relevant protocol or algorithm names. Explain their role alongside the name. Expand an unfamiliar abbreviation when its expansion helps; unpack the mechanism when the expansion alone teaches little.
4. **Develop one example across related concepts.** Keep the caller, resource and request recognizable as permissions, checks and failures enter the explanation. Show the result when an assumption fails. Add another example when it exposes a distinct case.
5. **Connect actions causally.** State who supplies an input, what checks it, what result follows and which later step consumes that result. Give a supported reason for a constraint; consult its Why or ADR instead of inventing a plausible rationale.
6. **Put limits beside claims.** Name the object proved, assumptions trusted and behavior outside the proof. A model property, agreement on generated cases and end-to-end enforcement establish different claims. Show a counterexample to the stronger interpretation when useful.
7. **Give technical artifacts an explanatory job.** A small code fragment, equation, request or configuration example demonstrates a point. Introduce its inputs and explain its output or consequence. Preserve the syntax and relevant conditions; references carry the exhaustive cases.
8. **Repeat to connect ideas.** Reuse a term consistently and briefly restate what an example establishes before extending it. Recaps earn space when they connect earlier reasoning to the next step. Remove sentences that only announce a section or repeat its heading.
9. **Allocate space by explanatory need.** A subtle exception may need more words than a large benefit. State importance explicitly; paragraph length does not rank design choices. Use full sentences with clear actors and concrete verbs.

## Examples for Contextful

| Compressed wording | Explanatory wording |
| --- | --- |
| A checkpoint admits authority. | A checkpoint is the code that verifies a caller's credential and produces the permissions used by reads and writes. See `authority.verify.admitted-authority`. |
| A sandboxed component imports HTTP. | A sandboxed component is a WebAssembly component implementing the connector interface. Memory limits and per-call deadlines bound its execution; network requests pass through the host. See `connector.export`, `connector.import` and `connector.package`. |
| A proof with a hole fails on its footprint. | Lean can compile a proof containing an unfinished step. The assumption audit detects that unfinished step and rejects the proof. See `assurance.audit-assumptions`. |

These examples illustrate explanatory wording. An edit checks the owning clauses and preserves their conditions. Guide prose reaches exact rules through clause pointers, following `corpus.guide`. Guide length and anatomy follow `corpus.guide.file`; this brief changes neither.

## Readings and what to observe

All entries are by Vitalik Buterin on his website. Priority: **should-read**. Informs: `corpus.guide`, `corpus.anatomy`, `corpus.rationale`. Question: How does a technical explanation preserve detail while giving readers the reasoning needed to understand it? The observations describe the essays; the editorial practices above adapt them to this repository.

| Article | Passage to inspect | Writing lesson |
| --- | --- | --- |
| [STARKs, Part I: Proofs with Polynomials (2017)](https://vitalik.eth.limo/general/2017/11/09/starks_part_1.html) | “A Somewhat Simple Example” and “Going Further” | Carries a range-checking example through transformations, then exposes an omitted attack and the need for proximity checking. |
| [Trust Models (2020)](https://vitalik.eth.limo/general/2020/08/20/trust.html) | Definition, numbered trust models, then liveness and safety | Gives an ambiguous term a working definition and distinguishes cases by assumptions and observable failure consequences. |
| [An approximate introduction to how zk-SNARKs are possible (2021)](https://vitalik.eth.limo/general/2021/01/26/snarks.html) | Random sampling, polynomials, commitments and recaps | Each mechanism addresses a remaining obstacle; notation stays visible, with prose interpreting it and recaps connecting the steps. |
| [The Limits to Blockchain Scalability (2021)](https://vitalik.eth.limo/general/2021/05/23/scaling.html) | Computing power, bandwidth, storage and interaction effects | Challenges intuitive capacity estimates with operational reasons, numerical examples and explicitly qualified approximations. |
| [The different types of ZK-EVMs (2022)](https://vitalik.eth.limo/general/2022/08/04/zkevm.html) | Overview chart and repeated advantage/disadvantage sections | Makes alternatives comparable through consistent criteria. The Type 4 discussion explicitly separates explanatory length from value judgment. |
| [Multidimensional gas pricing (2024)](https://vitalik.eth.limo/general/2024/05/09/multidim.html) | Initial transaction breakdown, pricing alternatives and sub-call limits | Connects an observable example to equations, translates equations into behavior, then identifies the compatibility problem left unresolved. |
| [Simplifying the L1 (2025)](https://vitalik.eth.limo/general/2025/05/03/simplel1.html) | Bitcoin explanation, VM transition and shared components | Uses ordinary causal sentences, retains named mechanisms and gives the design direction a concrete compatibility and transition strategy. |
| [A shallow dive into formal verification (2026)](https://vitalik.eth.limo/general/2026/05/18/fv.html) | Fibonacci proof, Lean version and verification failure cases | Connects human reasoning to formal notation and shows why proof scope and specification quality remain consequential. |

The essays supply examples of explanation. Contextful behavior, bounds and guarantees come from its contracts and evidence; the articles' technical estimates do not establish requirements here.

## Review before handoff

- Can the intended reader explain the mechanism's purpose and trace one input through its checks to an output?
- Does each necessary unfamiliar term have a role, explanation or prerequisite reference at first use?
- Are technology names, formats, units, interfaces, assumptions and error conditions preserved where relevant?
- Does the example show a failure consequence as well as successful operation?
- Do links resolve, and do the cited clauses support the exact claims and scope of the prose?
- Does each paragraph add reasoning, evidence, an example or a connection needed for the next step?

Keep the repository's affirmative present tense, domain neutrality and stable terminology. Teach the system's behavior without narrating the author's or agent's production process.
