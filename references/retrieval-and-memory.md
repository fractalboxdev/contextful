# Retrieval and memory

Hybrid lexical and vector ranking under access restriction, and a synthesized
memory of claims that revise, decay and ground answers. Evaluation of both runs
through `assurance.evaluate`.

## Retrieval

### An Analysis of Fusion Functions for Hybrid Retrieval

Bruch, S., Gai, S., Ingber, A. "An Analysis of Fusion Functions for Hybrid
Retrieval." *ACM TOIS* 42(1), Article 20, 2023. <https://arxiv.org/abs/2210.11934>

- **Priority:** must-read
- **Informs:** `read.rank`, `read.retrieve`, `assurance.evaluate`
- **Question:** Does a min-max convex combination with fixed 0.6 vector / 0.4 lexical
  weights beat reciprocal rank fusion on this corpus? Convex combination wins only
  with weights tuned per domain, and normalization matters less than tuning; the
  fixed default is a hypothesis until the evaluation harness reports it against RRF
  on labeled cases.

### Reciprocal Rank Fusion

Cormack, G. V., Clarke, C. L. A., Büttcher, S. "Reciprocal rank fusion outperforms
Condorcet and individual rank learning methods." *SIGIR*, pp. 758–759, 2009.
<https://dl.acm.org/doi/abs/10.1145/1571941.1572114>

- **Priority:** should-read
- **Informs:** `read.rank`
- **Question:** What baseline needs no calibration? RRF uses ranks only, so it needs
  no score normalization and no special case for a flat score window.

### ACORN

Patel, L., Kraft, P., Guestrin, C., Zaharia, M. "ACORN: Performant and
Predicate-Agnostic Search Over Vector Embeddings and Structured Data." *SIGMOD*,
2024. <https://arxiv.org/abs/2403.04871>

- **Priority:** optional
- **Informs:** `read.retrieve`, `store.index`
- **Question:** What replaces a fixed over-fetch multiplier under row restriction?
  Predicate-aware graph search keeps recall when the filter is selective, where a
  constant over-fetch either wastes work or returns short.

## Memory

### Zep

Rasmussen, P., Paliychuk, P., Beauvais, T., Ryan, J., Chalef, D. "Zep: A Temporal
Knowledge Graph Architecture for Agent Memory." arXiv:2501.13956, 2025.
<https://arxiv.org/abs/2501.13956>

- **Priority:** must-read
- **Informs:** `read.synthesize`, `read.revise`, `read.resolve-entity`, `read.recall`
- **Question:** How are multi-valued predicates and edge invalidation handled? Zep is
  the closest published design — episodes as a lossless source, extracted entities
  and edges, bitemporal invalidation. A supersession rule without declared predicate
  cardinality cannot represent a predicate with several true objects, a dedup key
  that omits the object conflates them, and a fixed 0.5 confidence decay has no
  measurement behind it where Zep invalidates by time.

### MemGPT and Generative Agents

Packer, C., Wooders, S., Lin, K., Fang, V., Patil, S. G., Stoica, I.,
Gonzalez, J. E. "MemGPT: Towards LLMs as Operating Systems." arXiv:2310.08560, 2023.
<https://arxiv.org/abs/2310.08560>

Park, J. S., O'Brien, J., Cai, C. J., Morris, M. R., Liang, P., Bernstein, M. S.
"Generative Agents: Interactive Simulacra of Human Behavior." *UIST*, 2023.
<https://dl.acm.org/doi/10.1145/3586183.3606763>

- **Priority:** optional
- **Informs:** `read.recall`, `surface.learn`
- **Question:** What reference designs sit behind tier-first recall and per-turn
  distillation? Tiered memory (MemGPT) and recency/importance/relevance retrieval
  with reflection (Generative Agents).

### LongMemEval

Wu, D., Wang, H., Yu, W., Zhang, Y., Chang, K.-W., Yu, D. "LongMemEval:
Benchmarking Chat Assistants on Long-Term Interactive Memory." *ICLR*, 2025.
<https://arxiv.org/abs/2410.10813>

- **Priority:** should-read
- **Informs:** `assurance.evaluate`, `read.revise`, `read.recall`, `store.bound-time`
- **Question:** What does a memory evaluation suite measure beyond retrieval? Five
  abilities — extraction, multi-session reasoning, temporal reasoning, knowledge
  update, abstention — of which the last three map onto revision, time bounds and the
  recall evidence gate.

### AgentPoison

Chen, Z., Xiang, Z., Xiao, C., Song, D., Li, B. "AgentPoison: Red-teaming LLM Agents
via Poisoning Memory or Knowledge Bases." *NeurIPS*, 2024.
<https://arxiv.org/abs/2407.12784>

- **Priority:** should-read
- **Informs:** `read.synthesize`, `read.recall`, `authority.resist`, `surface.learn`
- **Question:** Should provenance tier be a hard gate on recall rather than a ranking
  signal? Extraction turns ingested third-party text into durable claims that later
  ground answers, which is exactly the path trigger-optimized poisoning targets.
  The same concern covers distillation: a conclusion drawn from one reader's rows
  carries those rows' labels to any later reader.
