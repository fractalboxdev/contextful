# Specification practice

How the corpus itself is written: clause-addressed rules, decision records, requirement
syntax, lint rules over prose, and the link from a clause to the test, theorem or type
that demonstrates it.

## Practices

| Practice | Source | Informs | What the corpus takes from it |
| --- | --- | --- | --- |
| Rule id to test annotation: Rust Reference `r[...]` ids, compiletest `//@ reference:`, the Ferrocene FLS traceability matrix | <https://public-docs.ferrocene.dev/main/qualification/plan/details.html>; *Ferrocene Language Specification* below | `corpus.state` | Tests carry `#[spec("…")]` and the checker collects them; the central pin file holds only theorem and type pins, so no second map drifts. |
| Coverage types and revision suffix (OpenFastTrace `impl~`/`utest~`, `~1`; Doorstop; sphinx-needs) | <https://github.com/itsallcode/openfasttrace> · <https://github.com/doorstop-dev/doorstop> · <https://sphinx-needs.readthedocs.io> | `corpus.state`, `corpus.address` | Each kind declares the coverage it needs (refusal and limit need a test or theorem, shape needs a type), and a pin records a statement hash so a reworded clause loses its pin instead of keeping it silently. |
| ADR granularity; Mega-ADR, Dummy Alternative and Blueprint-in-disguise anti-patterns | <https://www.ozimmer.ch/practices/2023/04/03/ADRCreation.html> · <https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions> | `corpus.rationale` | One record per architecturally significant decision, not per refusal; a refusal's direction follows from a small set of principles; options are real alternatives, never straw men. |
| MADR 4 minimal template plus Confirmation | <https://adr.github.io/madr/> | `corpus.rationale` | Context, options and outcome are mandatory, the rest optional; a Confirmation line names the pin that proves the decision holds. |
| Y-statements | <https://medium.com/olzzio/y-statements-10eb07b5a177> · <https://socadk.github.io/design-practice-repository/artifact-templates/DPR-ArchitecturalDecisionRecordYForm.html> | `corpus.rationale`, `corpus.anatomy` | A one-sentence `because` cell carries a single-clause decision; a full record is kept only where the options genuinely diverge. |
| RFC 2119 / RFC 8174 keywords | <https://www.rfc-editor.org/rfc/rfc8174> | `corpus.anatomy`, `corpus.rationale` | Position replaces keywords — only a clause cell is normative, and a stray modal is refused — which is stronger than RFC 8174; optional or conditional behavior needs its own form, since every clause is absolute. |
| EARS templates | <https://alistairmavin.com/ears/> | `corpus.address`, `corpus.anatomy` | A refusal is the unwanted-behavior pattern ("If <trigger>, the <party> raises <Error>") with a parseable trigger, a workflow the event- and state-driven patterns, and the kind segment is computed from the form. |
| ISO/IEC/IEEE 29148 requirement quality: singular, verifiable, necessary | <https://www.iso.org/standard/72089.html> | `corpus.anatomy`, `corpus.state` | One obligation per row under a clause-word cap; a limit names its workload; a shape row that a type or DDL pin already carries is not necessary. |
| test262 beside ecma262; ESMeta; WHATWG living standards | <https://github.com/tc39/test262> · <https://whatwg.org/faq>; *JISET and JEST* below | `corpus.rationale`, `assurance.test` | The specification stays short because an executable conformance suite carries the edge cases; parser, matcher and scanner detail moves into fixture tests. |
| KEP and Rust RFC templates | <https://github.com/kubernetes/enhancements/blob/master/keps/NNNN-kep-template/README.md> · <https://github.com/rust-lang/rfcs/blob/master/0000-template.md> | `corpus.rationale` | Rationale for one operation lives in one document with alternatives, drawbacks and a test plan, rather than in many per-clause records. |
| Oxide RFD lifecycle | <https://rfd.shared.oxide.computer/rfd/0001> | `corpus.rationale`, `corpus.state` | Lifecycle state is the only place a date could belong; the grammar either legalizes it in one field or drops it, never both. |
| Requirements smell detection with measured precision | *Rapid quality assurance with Requirements Smells* and *Requirements quality research* below | `corpus.state`, `corpus.rationale`, `corpus.reference` | The checker emits per-rule hit and override counts, and a lint rule stays only while its false-refusal rate and downstream defect effect are measured. |
| Executable models for the protocol-critical core | see [formal-methods.md](./formal-methods.md) | `store.lease`, `store.fold`, `run.journal` | A TLA+ or P model of lease + CAS + fence, with the store refusals pinned to its invariants. |
| Fencing checked by the resource; S3 conditional writes | see [leases-and-coordination.md](./leases-and-coordination.md), [storage-and-table-formats.md](./storage-and-table-formats.md) | `store.lease`, `store.probe` | The probe names `If-None-Match` and `If-Match`; the store, not the writer, rejects a stale fence. |
| DPoP; Merkle transparency log; JCS canonical JSON | see [access-control-and-capabilities.md](./access-control-and-capabilities.md), [audit-and-erasure.md](./audit-and-erasure.md), [durable-execution.md](./durable-execution.md) | `authority.verify`, `disclosure.attest`, `run.declare` | Skew and replay limits from RFC 9449; a Merkle tree per segment from RFC 6962; the declaration hash defined by RFC 8785. |

## Decision records

### Sustainable Architectural Design Decisions

Zdun, U., Capilla, R., Tran, H., Zimmermann, O. "Sustainable Architectural Design
Decisions." *IEEE Software* 30(6), 2013. <https://doi.org/10.1109/MS.2013.97>

- **Priority:** must-read
- **Informs:** `corpus.rationale`, `corpus.anatomy`
- **Question:** What is the smallest complete form of a decision? The Y-statement —
  in the context of X, facing Y, we chose Z over W to achieve Q, accepting D — is the
  model for the `because` cell and for folding single-clause records into it.

### A survey of architecture design rationale

Tang, A., Babar, M. A., Gorton, I., Han, J. "A survey of architecture design
rationale." *Journal of Systems and Software* 79(12), 2006.
<https://doi.org/10.1016/j.jss.2006.04.029>

- **Priority:** must-read
- **Informs:** `corpus.rationale`
- **Question:** How much captured rationale is ever read? The survey measures the
  cost of capture against the rate of use and names why rationale goes unmaintained —
  the central risk when rationale outweighs behavior several times over.

### Using Architecture Decision Records in Open Source Projects

Buchgeher, G., Schöberl, S., Geist, V., Dorninger, B., Haindl, P., Weinreich, R.
"Using Architecture Decision Records in Open Source Projects — An MSR Study on
GitHub." *IEEE Access* 11:63725–63740, 2023.
<https://se.jku.at/using-architecture-decision-records-in-open-source-projects-an-msr-study-on-github/>

- **Priority:** should-read
- **Informs:** `corpus.rationale`
- **Question:** What record count and size do projects that sustain ADRs keep?
  Sustained use is sparse and per significant decision, a baseline for judging a
  record-per-refusal rule.

### Architecture Decision Records in Practice

Ahmeti, B., Linder, M., Groner, R., Wohlrab, R. "Architecture Decision Records in
Practice: An Action Research Study." *ECSA*, 2024.
<https://rebekkaa.github.io/files/2024_ECSA.pdf>

- **Priority:** should-read
- **Informs:** `corpus.rationale`
- **Question:** What makes records useful or ignored in a working team? Documentation
  culture, knowledge transfer and prioritization of what to record are addressed by
  ADRs; decisions about shared and distributed components are not, and where records
  are stored has a large effect on their perceived usefulness.

### Kinds and documentation of design decisions in practice

Weinreich, R., Groher, I., Miesbauer, C. "An expert survey on kinds, influence
factors and documentation of design decisions in practice." *Future Generation
Computer Systems* 47, 2015. <https://doi.org/10.1016/j.future.2014.12.002>

- **Priority:** optional
- **Informs:** `corpus.rationale`
- **Question:** Which decision kinds do practitioners document? The survey supports a
  two-tier model: principles plus significant decisions.

## Requirement statements and their quality

### Easy Approach to Requirements Syntax

Mavin, A., Wilkinson, P., Harwood, A., Novak, M. "Easy Approach to Requirements
Syntax (EARS)." *IEEE RE*, 2009. <https://doi.org/10.1109/RE.2009.9>

- **Priority:** must-read
- **Informs:** `corpus.address`, `corpus.anatomy`
- **Question:** Should a clause's kind be computed from its sentence form? Five
  templates — ubiquitous, event-driven, state-driven, unwanted behavior, optional —
  cover most requirements and are checkable, which turns free-prose invariants and
  refusals into trigger and response fields a checker and a test generator read.

### Rapid quality assurance with Requirements Smells

Femmer, H., Méndez Fernández, D., Wagner, S., Eder, S. "Rapid quality assurance with
Requirements Smells." *Journal of Systems and Software* 123:190–213, 2017.
<https://wwwbroy.in.tum.de/~femmer/works/2016-requirements_smells-jss.pdf>

- **Priority:** must-read
- **Informs:** `corpus.state`, `corpus.rationale`, `corpus.reference`
- **Question:** What false-refusal rate is acceptable for the lint? Automated smell
  detection reached 59% precision and 82% recall; the corpus checker is a smell
  detector in the same tradition, and its token lists (rationale words, modals,
  dated vocabulary) need the same measurement, including the markers they miss
  (`since`, `, so`).

### Requirements quality research

Frattini, J., Montgomery, L., Fischbach, J., Mendez, D., Fucci, D., Unterkalmsteiner,
M. "Requirements quality research: a harmonized theory, evaluation, and roadmap."
*Requirements Engineering* 28:507–520, 2023. <https://arxiv.org/abs/2309.10355>

- **Priority:** should-read
- **Informs:** `corpus.state`, `corpus.anatomy`
- **Question:** Does a grammar rule reduce defects downstream? A quality rule earns its
  authoring cost only through measured impact, which is the test every corpus rule
  answers to.

### Four dark corners of requirements engineering

Zave, P., Jackson, M. "Four dark corners of requirements engineering." *ACM TOSEM*
6(1), 1997. <https://doi.org/10.1145/237432.237434>

- **Priority:** should-read
- **Informs:** `corpus.anatomy`, `topology.compose`
- **Question:** Where does an environment assumption end and the machine specification
  begin? Parties rows and record Context sections mix the two; the split tells which
  sentences are obligations and which are assumptions a deployment must hold.

### Naming the pain in requirements engineering

Méndez Fernández, D., Wagner, S., et al. "Naming the pain in requirements
engineering: Contemporary problems, causes, and effects in practice." *Empirical
Software Engineering* 22, 2017. <https://doi.org/10.1007/s10664-016-9451-7>

- **Priority:** optional
- **Informs:** `corpus.rationale`, `corpus.anatomy`
- **Question:** Where does the word budget belong? Incomplete and underspecified
  requirements, not missing rationale, cause the most project failure.

## Traceability and mechanization

### Ferrocene Language Specification and the Rust specification RFC

Ferrous Systems. "Ferrocene Language Specification." <https://spec.ferrocene.dev>

Rust Project. "RFC 3355: Rust Specification."
<https://rust-lang.github.io/rfcs/3355-rust-spec.html>

- **Priority:** should-read
- **Informs:** `corpus.state`, `corpus.address`
- **Question:** How does a paragraph id reach a test in the same toolchain? Paragraph
  ids, in-test annotations and a generated traceability matrix are the working model
  for replacing a central pin map with annotations the checker collects.

### JISET and JEST

Park, J., Park, J., An, S., Ryu, S. "JISET: JavaScript IR-based Semantics Extraction
Toolchain." *ASE*, 2020; Park, J., An, S., Youn, D., Kim, G., Ryu, S. "JEST: N+1-version
Differential Testing of Both JavaScript Engines and Specification." *ICSE*, 2021.
<https://github.com/es-meta/esmeta>

- **Priority:** optional
- **Informs:** `corpus.state`, `assurance.test`
- **Question:** Can structured clause rows generate test skeletons? JISET mechanizes
  prose algorithm steps and JEST runs the result differentially against engines and
  the specification text, catching defects on both sides.
