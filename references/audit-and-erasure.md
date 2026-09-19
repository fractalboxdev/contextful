# Audit and erasure

The audit chain every read appends to, signed roots over it, erasure by tombstone
and lineage cascade, and the receipt a purge returns. Keyed digests over
identifiers live in [privacy-and-disclosure.md](./privacy-and-disclosure.md).

## Tamper-evident logs

### Efficient Data Structures for Tamper-Evident Logging

Crosby, S. A., Wallach, D. S. "Efficient Data Structures for Tamper-Evident
Logging." *USENIX Security*, pp. 317–334, 2009.
<https://static.usenix.org/event/sec09/tech/full_papers/crosby.pdf>

- **Priority:** must-read
- **Informs:** `disclosure.record`, `disclosure.attest`, `disclosure.erase`, `disclosure.receipt`
- **Question:** How does the chain give O(log n) membership and consistency proofs,
  and delete old events without breaking them? A linear
  `sha256(seq ‖ prev_hash ‖ attributes)` chain verifies only by a full walk and
  cannot prove a receipt's inclusion compactly; a history tree does both. The same
  model treats an absent segment while a tip or signed root exists as a broken chain,
  never as a fresh genesis.

### Certificate Transparency

Laurie, B., Langley, A., Kasper, E. "Certificate Transparency." RFC 6962, 2013.
<https://www.rfc-editor.org/rfc/rfc6962>

- **Priority:** should-read
- **Informs:** `disclosure.attest`, `disclosure.record`
- **Question:** What does a Merkle tree per segment with signed tree heads buy?
  Inclusion proofs for a receipt and consistency proofs between segments, verifiable
  by a party holding no credential.

### Secure audit logs to support computer forensics

Schneier, B., Kelsey, J. "Secure audit logs to support computer forensics." *ACM
TISSEC* 2(2):159–176, 1999. <https://dl.acm.org/doi/10.1145/317087.317089>

- **Priority:** optional
- **Informs:** `disclosure.attest`
- **Question:** Who holds the key that signs segment roots, relative to the machine
  that writes the chain? Forward-secure sealing keeps a compromised logger from
  rewriting entries made before the compromise.

## Erasure

### Understanding and Benchmarking the Impact of GDPR on Database Systems

Shastri, S., Banakar, V., Wasserman, M., Kumar, A., Chidambaram, V. "Understanding
and Benchmarking the Impact of GDPR on Database Systems." *PVLDB* 13(7):1064–1077,
2020. <http://www.vldb.org/pvldb/vol13/p1064-shastri.pdf>

- **Priority:** should-read
- **Informs:** `disclosure.erase`, `disclosure.receipt`, `disclosure.erase`
- **Question:** Which system capabilities does `forget` need? The paper maps GDPR
  articles to capabilities and measures the metadata cost; GDPRbench is a cost
  baseline for tombstone, cascade and rewrite.

### Lethe

Sarkar, S., Papon, T. I., Staratzis, D., Athanassoulis, M. "Lethe: A Tunable
Delete-Aware LSM Engine." *SIGMOD*, 2020. <https://dl.acm.org/doi/10.1145/3318464.3389757>

- **Priority:** should-read
- **Informs:** `disclosure.erase`, `disclosure.receipt`, `store.fold`, `disclosure.erase`
- **Question:** How long does a tombstoned value stay physically present? Tombstones
  in run files persist until the next fold and the run-retention window, and a
  bounded `as_of` read still reaches superseded snapshots. Lethe bounds
  delete-persistence latency; the receipt's claim scope depends on the same bound,
  stated as a limit.

### Provenance Semirings

Green, T. J., Karvounarakis, G., Tannen, V. "Provenance Semirings." *PODS*,
pp. 31–40, 2007. <https://web.cs.ucdavis.edu/~green/papers/pods07.pdf>

- **Priority:** should-read
- **Informs:** `disclosure.erase`, `disclosure.erase`, `read.recall`, `read.synthesize`
- **Question:** When is a one-hop lineage cascade sound? Evidence row ids on claims,
  the erasure cascade and derive-tier citation keys are why-provenance; one hop
  reaches everything derived only when every derived row records its complete
  witness set, and multi-hop synthesis needs the transitive closure or a claim
  restated as one hop.

### Redactable Blockchain

Ateniese, G., Magri, B., Venturi, D., Andrade, E. "Redactable Blockchain – or –
Rewriting History in Bitcoin and Friends." *IEEE EuroS&P*, pp. 111–126, 2017.
<https://dblp.org/rec/conf/eurosp/AtenieseM0A17.html>

- **Priority:** optional
- **Informs:** `disclosure.record`, `disclosure.erase`
- **Question:** What happens when an erasure request covers audit spans that carry a
  reader's subject tuple? Chameleon-hash redaction rewrites a hash-linked entry
  without breaking the chain.
