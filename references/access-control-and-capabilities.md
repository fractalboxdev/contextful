# Access control and capabilities

Enforcement as a rewrite of caller SQL, source permissions mirrored with a
staleness budget, credentials that attenuate offline, and connectors with no
ambient authority. Keyed pseudonyms and masking digests live in
[privacy-and-disclosure.md](./privacy-and-disclosure.md); the Cedar verification
work lives in [formal-methods.md](./formal-methods.md).

## Query rewriting

### Extending Query Rewriting Techniques for Fine-Grained Access Control

Rizvi, S., Mendelzon, A., Sudarshan, S., Roy, P. "Extending Query Rewriting
Techniques for Fine-Grained Access Control." *SIGMOD*, pp. 551–562, 2004.
<https://dl.acm.org/doi/10.1145/1007568.1007631>

- **Priority:** must-read
- **Informs:** `authority.compose`, `authority.refuse`, `authority.mask`, `read.guard`
- **Question:** Truman or non-Truman, per surface? Silently narrowing a relation is
  the Truman model; raising a scope refusal instead of an empty result is the
  non-Truman model. The paper shows the Truman model returning misleading
  aggregates, which is the case for refusing empty-result substitution, and gives
  the vocabulary to name which model each surface — relation wrapping, masks under
  `SELECT *`, template relations — commits to.

### Access control by query modification

Stonebraker, M., Wong, E. "Access control in a relational data base management
system by query modification." *ACM Annual Conference*, pp. 180–186, 1974.
<https://dl.acm.org/doi/10.1145/800182.810400>

- **Priority:** optional
- **Informs:** `authority.compose`
- **Question:** Where does "protection is a rewrite" come from? The origin of
  enforcing row restriction by modifying the query rather than filtering results.

### Blockaid

Zhang, W., Sheng, E., Chang, M. A., Panda, A., Sagiv, M., Shenker, S. "Blockaid:
Data Access Policy Enforcement for Web Applications." *OSDI*, 2022.
<https://www.usenix.org/conference/osdi22/presentation/zhang>

- **Priority:** optional
- **Informs:** `read.guard`, `authority.refuse`
- **Question:** What would a complete decision procedure cost? Blockaid proves each
  query policy-compliant with SMT and caches generalized decisions — the alternative
  to a guard that decides only top-level conjuncts under a bounded node walk.

## Relationship-based access and staleness

### Zanzibar

Pang, R., et al. "Zanzibar: Google's Consistent, Global Authorization System."
*USENIX ATC*, 2019. <https://www.usenix.org/conference/atc19/presentation/pang>

- **Priority:** must-read
- **Informs:** `disclosure.bound-staleness`, `disclosure.reach`, `disclosure.sweep`, `disclosure.mirror`
- **Question:** Is a per-table age budget against a sweep watermark an acceptable
  answer to the new-enemy problem? Zanzibar orders ACL updates causally against
  content with zookies; the budget is a weaker guarantee, and naming it as such is the
  decision. The Leopard index is the published answer to nested-group closure —
  cycles, breadth and cache bounds included — which the depth-bounded reach
  closure also has to state.

## Capability tokens

### Macaroons

Birgisson, A., Politz, J. G., Erlingsson, Ú., Taly, A., Vrable, M., Lentczner, M.
"Macaroons: Cookies with Contextual Caveats for Decentralized Authorization in the
Cloud." *NDSS*, 2014. <https://research.google/pubs/pub41892/>

- **Priority:** must-read
- **Informs:** `authority.profile`, `authority.attenuate`, `authority.verify`
- **Question:** Which chain format supports N-hop attenuation? Offline attenuation by
  appended caveats is the model the delegation profile implements; a wire envelope
  with room for one attenuation segment cannot carry a grandchild, so the design
  either adopts the library's chain format or specifies an N-block grammar with a
  depth limit. The caveat-discharge analysis also explains why the profile refuses
  third-party blocks.

### Biscuit

Biscuit authors. "Biscuit authorization token specification."
<https://doc.biscuitsec.org>

- **Priority:** should-read
- **Informs:** `authority.profile`, `authority.attenuate`, `authority.verify`
- **Question:** Which Biscuit revision does a profile version pin? Appended blocks
  that only narrow, authorizer-supplied reserved facts and an evaluator under
  iteration ceilings are Biscuit semantics; the profile version names the upstream
  specification revision it depends on.

### Robust Composition

Miller, M. S. "Robust Composition: Towards a Unified Approach to Access Control and
Concurrency Control." PhD thesis, Johns Hopkins University, 2006.
<http://www.erights.org/talks/thesis/>

- **Priority:** should-read
- **Informs:** `authority.attenuate`, `authority.grant`, `connector.import`, `connector.declare-capability`, `connector.resolve`, `run.cancel`
- **Question:** What does "authority as a value" and "no ambient authority" commit
  the design to? The vocabulary — designation carries authority, delegation only
  attenuates — turns the host-mediated connector imports and the credential
  resolution chain into testable invariants; an environment variable that shadows
  the configured manager is ambient authority by this definition.

### DPoP

Fett, D., Campbell, B., Bradley, J., Lodderstedt, T., Jones, M., Waite, D. "OAuth
2.0 Demonstrating Proof of Possession (DPoP)." RFC 9449, 2023.
<https://www.rfc-editor.org/rfc/rfc9449>

- **Priority:** should-read
- **Informs:** `authority.verify`, `authority.revoke`
- **Question:** What clock-skew tolerance, replay window and nonce-cache bound does a
  possession proof need? The possession-proof and replayed-nonce refusals re-derive
  DPoP; the RFC supplies the parameters those limits align with.

## Sandboxed connectors

### Provably-Safe Multilingual Software Sandboxing using WebAssembly

Bosamiya, J., Lim, W. S., Parno, B. "Provably-Safe Multilingual Software Sandboxing
using WebAssembly." *USENIX Security*, 2022.
<https://www.usenix.org/conference/usenixsecurity22/presentation/bosamiya>

- **Priority:** should-read
- **Informs:** `connector.import`, `connector.package`, `assurance.scope-claim`
- **Question:** What sits in the trusted computing base when a component boundary is
  the isolation? Wasm isolation is as strong as the runtime and compiler enforcing
  it; the paper names what the scope claim's trusted-dependency list includes, and
  whether a compiled-in native sibling can share the component's trust level.

### The WebAssembly Component Model

Bytecode Alliance. "The WebAssembly Component Model."
<https://component-model.bytecodealliance.org/>

- **Priority:** should-read
- **Informs:** `connector.export`, `connector.package`
- **Question:** Is a world version bump additive-compatible? World and interface
  versioning follows the component model's semver-on-interface and subtyping rules
  rather than an in-house scheme.
