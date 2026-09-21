# Privacy and disclosure

Aggregate release under a disclosure budget, suppression floors, sanctioned
declassification, and digests that claim to name nobody. Tamper-evident logging and
erasure live in [audit-and-erasure.md](./audit-and-erasure.md).

### Differentially Private SQL with Bounded User Contribution

Wilson, R. J., Zhang, C. Y., Lam, W., Desfontaines, D., Simmons-Marengo, D.,
Gipson, B. "Differentially Private SQL with Bounded User Contribution." *PoPETs*
2020(2):230–250. <https://petsymposium.org/popets/2020/popets-2020-0025.pdf>

- **Priority:** must-read
- **Informs:** `disclosure.release`, `disclosure.suppress`, `disclosure.bound-cohort`
- **Question:** How is thresholding made private? The release pipeline follows this
  design — bound per-person contribution, threshold, add noise, with the person and
  not the row as the privacy unit — but suppressing groups on exact counts before
  noise is not differentially private. The paper's noisy thresholding (DP partition
  selection) is the fix, and its budget accounting argues for reserving spend in one
  transaction before the release runs, so concurrent releases cannot overspend.

### Revealing information while preserving privacy

Dinur, I., Nissim, K. "Revealing information while preserving privacy." *PODS*,
pp. 202–210, 2003. <https://dl.acm.org/doi/10.1145/773153.773173>

- **Priority:** should-read
- **Informs:** `disclosure.suppress`, `disclosure.release`, `disclosure.bound-cohort`
- **Question:** Why can floors plus a dominance rule not be the privacy boundary?
  Answering many subset-sum queries with perturbation below √n lets the data be
  reconstructed; repeated releases are exactly that query stream, so the noise step
  stays mandatory for statistics releases.

### Declassification: Dimensions and principles

Sabelfeld, A., Sands, D. "Declassification: Dimensions and principles." *Journal of
Computer Security* 17(5):517–548, 2009.
<https://journals.sagepub.com/doi/10.3233/JCS-2009-0352>

- **Priority:** should-read
- **Informs:** `disclosure.release`, `surface.publish-answer`, `disclosure.template`
- **Question:** What does each sanctioned release declassify, by whom, where and
  when? The four dimensions classify aggregate release, a published answer (the
  asker's act) and a template relation running raw under a capability in one
  framing, so the records behind them share it instead of restating it.

### The Pitfalls of Hashing for Privacy

Demir, L., Kumar, A., Cunche, M., Lauradoux, C. "The Pitfalls of Hashing for
Privacy." *IEEE Communications Surveys & Tutorials* 20(1):551–565, 2018.
<https://inria.hal.science/hal-01589210>

- **Priority:** must-read
- **Informs:** `authority.mask`, `disclosure.erase`, `disclosure.receipt`, `disclosure.record`
- **Question:** Which digests need a key? An unkeyed SHA-256 over an email or other
  low-entropy identifier reverses by dictionary, so the erasure ledger, the receipt's
  tenant hash and the query digest are pseudonymous only under a keyed hash. The same
  analysis bounds truncation: a prefix width is safe only when the class domain
  leaves at least k candidates per bucket, which caps the width at
  `floor(log16(domain / k))` hex digits. A masking relation that embeds key material
  in SQL text lets any catalog reader compute the keyed hash, so the key lives in a
  native function, never in a relation body.
