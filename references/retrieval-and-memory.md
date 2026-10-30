# Retrieval and memory

Hybrid lexical and vector ranking under access restriction, graph retrieval over
extracted entities, and a synthesized memory of claims that revise, decay and
ground answers. Evaluation of all three runs through `assurance.evaluate`.

## Retrieval

### An Analysis of Fusion Functions for Hybrid Retrieval

Bruch, S., Gai, S., Ingber, A. "An Analysis of Fusion Functions for Hybrid
Retrieval." *ACM TOIS* 42(1), Article 20, 2023. <https://arxiv.org/abs/2210.11934>

- **Priority:** must-read
- **Informs:** `read.rank`, `read.retrieve`, `assurance.evaluate`
- **Question:** Does a convex combination over min-max-normalized scores, its weight
  fitted per domain on settled judgments, beat the fused score of `read.rank`, which
  adds a clamped raw cosine to a reciprocal-rank score of the BM25 arm at fixed 60/40
  weights? Convex combination wins only with weights tuned per domain, and
  normalization matters less than tuning; a sum of a raw score and a rank score is
  neither scheme, so the evaluation harness settles the choice against both on
  labeled cases.

### Reciprocal Rank Fusion

Cormack, G. V., Clarke, C. L. A., Büttcher, S. "Reciprocal rank fusion outperforms
Condorcet and individual rank learning methods." *SIGIR*, pp. 758–759, 2009.
<https://dl.acm.org/doi/abs/10.1145/1571941.1572114>

- **Priority:** should-read
- **Informs:** `read.rank`
- **Question:** What baseline needs no calibration? RRF uses ranks only, so it needs
  no score normalization and no special case for a flat score window.

### Contextual Retrieval

Anthropic. "Introducing Contextual Retrieval." *Anthropic news*, 2024.
<https://www.anthropic.com/news/contextual-retrieval>

- **Priority:** should-read
- **Informs:** `read.rank`, `read.retrieve`, `read.cache`, `assurance.evaluate`
- **Question:** What does a second-stage reranker buy over hybrid retrieval? Top-20
  retrieval failures fall from 2.9% with contextual embeddings plus contextual BM25 to
  1.9% once a reranker reorders the fused candidates. A reranker that reads only the
  masked snippet of a post-authority window keeps that gain without reading hidden
  cells; contextual chunk headers written over rows of mixed authority encode hidden
  content into a visible vector.

### ACORN

Patel, L., Kraft, P., Guestrin, C., Zaharia, M. "ACORN: Performant and
Predicate-Agnostic Search Over Vector Embeddings and Structured Data." *SIGMOD*,
2024. <https://arxiv.org/abs/2403.04871>

- **Priority:** optional
- **Informs:** `read.retrieve`, `store.index`, `assurance.evaluate`
- **Question:** Do ACORN's predicate-agnostic expanded neighbour lists lower the cost
  of `read.retrieve.filtered-traversal` under a narrow filter enough to move the
  crossover `read.retrieve.filtered-exact-threshold` fixes? Filtered recall@10 and
  probe latency against the exact path, at the matching counts around that crossover,
  settle the build-time cost.

## Graph retrieval

### GraphRAG

Edge, D., Trinh, H., Cheng, N., Bradley, J., Chao, A., Mody, A., Truitt, S.,
Metropolitansky, D., Ness, R. O., Larson, J. "From Local to Global: A Graph RAG
Approach to Query-Focused Summarization." arXiv:2404.16130, 2024.
<https://arxiv.org/abs/2404.16130>

Microsoft. "Configuration: settings.yaml." *GraphRAG documentation*.
<https://microsoft.github.io/graphrag/config/yaml/>

- **Priority:** should-read
- **Informs:** `read.synthesize`, `read.declare`, `disclosure.erase`
- **Question:** What do gleaning and an LLM call cache cost and buy in extraction?
  Gleaning re-prompts a batch for entities the first pass omitted, with
  `max_gleanings = 1` as the default, and the cache keeps every model response so a
  re-run makes no new call. Open for the decision: whether a declared, single,
  optional pass bounds the first, and whether a cache keyed on the batch bytes inside their data boundary markers,
  template and model, held as an enforced table, keeps the second inside the store's
  visibility and erasure. Community summaries carry no per-sentence citation and are
  not adopted.

### LightRAG

Guo, Z., Xia, L., Yu, Y., Ao, T., Huang, C. "LightRAG: Simple and Fast
Retrieval-Augmented Generation." arXiv:2410.05779, 2024.
<https://arxiv.org/abs/2410.05779> · <https://github.com/HKUDS/LightRAG>

- **Priority:** should-read
- **Informs:** `read.synthesize`, `disclosure.erase`
- **Question:** How does an extracted graph recover after a source document is
  deleted? LightRAG rebuilds the affected entities and relations from the indexing
  LLM cache instead of re-extracting the corpus. Open for the decision: should
  restaging the claims that survive an erasure reuse cached responses for untouched
  batches the same way, with the cache entries keyed on the erased row collected with
  it?

### HippoRAG 2

Jiménez Gutiérrez, B., Shu, Y., Qi, W., Zhou, S., Su, Y. "From RAG to Memory:
Non-Parametric Continual Learning for Large Language Models." *ICML*, 2025.
<https://arxiv.org/abs/2502.14802>

- **Priority:** should-read
- **Informs:** `read.resolve-entity`, `read.rank`, `read.retrieve`
- **Question:** What is the cheapest graph retrieval that wins on multi-hop questions?
  Personalized PageRank seeded from the query's resolved entities over an
  entity-passage graph. Whether such a graph arm fuses as a third arm beside the
  lexical and vector arms, and at what weight, is the question a bounded walk over
  admitted memory edges leaves open.

### GraphRAG-Bench

Xiang, Z., Wu, C., Zhang, Q., Chen, S., Hong, Z., Huang, X., Su, J. "When to use
Graphs in RAG: A Comprehensive Analysis for Graph Retrieval-Augmented Generation."
arXiv:2506.05690, 2025. <https://arxiv.org/abs/2506.05690>

- **Priority:** should-read
- **Informs:** `assurance.evaluate`, `read.rank`
- **Question:** When does graph retrieval pay for itself? Only on multi-hop and
  summarization tasks: Personalized PageRank costs about 1,008 prompt tokens per query
  against 879 for plain RAG, while community-summary global search costs 38k to 331k.
  A graph walk earns its place by bounded walk depth and edge count per call, measured per task
  level rather than as one average.

## Memory

### Zep

Rasmussen, P., Paliychuk, P., Beauvais, T., Ryan, J., Chalef, D. "Zep: A Temporal
Knowledge Graph Architecture for Agent Memory." arXiv:2501.13956, 2025.
<https://arxiv.org/abs/2501.13956>

- **Priority:** must-read
- **Informs:** `read.synthesize`, `read.revise`, `read.declare`, `read.resolve-entity`, `read.recall`
- **Question:** How does a new edge retire the one it contradicts? Zep stores edges
  with validity intervals and invalidates the loser by time instead of deleting it.
  Open for the decision: should a one-cardinality relation let a new edge from one
  source end the prior live edge at its own `valid_from` and name it as successor,
  since a predicate with no declared cardinality does not say whether it holds one true
  object or more? Graphiti's newest-wins rule ignores tier and is not
  adopted.

### MemTX

Li, X., Wang, Y., Lu, H., Chen, Z., Li, M., Song, P., Zheng, M., Cai, T. "MemTX:
Transactional Belief Commit for Stateful Agent Memory." arXiv:2607.23929, 2026.
<https://arxiv.org/abs/2607.23929>

- **Priority:** must-read
- **Informs:** `read.revise`, `read.synthesize`, `read.recall`, `disclosure.erase`
- **Question:** When does a memory write become a belief? MemTX stages writes and
  admits them through validate-and-commit: a higher-authority candidate supersedes,
  a lower one aborts, and MemTX quarantines equal authority from different sources
  for review. A retraction walks the derivation graph and invalidates the summaries and
  index entries built on it, which is the cascade an erasure takes through claims and
  sidecars.

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
- **Informs:** `assurance.evaluate`, `assurance.baseline`, `read.revise`, `read.recall`, `store.bound-time`
- **Question:** What does a memory evaluation suite measure beyond retrieval? Five
  abilities — extraction, multi-session reasoning, temporal reasoning, knowledge
  update, abstention. Temporal reasoning reads at `observed_at`; abstention meets the
  recall evidence gate. Open for the decision: should knowledge update become a
  supersession-correctness measure, the rate of superseded claims served at their
  successor's anchor? The evaluation report
  lists a converted public set, and that set decides nothing beside the native cases.

### MemoryAgentBench

Hu, Y., Wang, Y., McAuley, J. "Evaluating Memory in LLM Agents via Incremental
Multi-Turn Interactions." arXiv:2507.05257, 2025.
<https://arxiv.org/abs/2507.05257>

- **Priority:** should-read
- **Informs:** `assurance.evaluate`, `read.revise`, `disclosure.erase`
- **Question:** Which memory competencies does a long-conversation benchmark miss?
  Accurate retrieval, test-time learning, long-range understanding and selective
  forgetting, with fact-consolidation tasks where a later fact overrides an earlier
  one. Its conflict-resolution and forgetting cases test supersession and erasure
  more directly than single-score benchmarks, and convert into tagged cases.

### AgentPoison

Chen, Z., Xiang, Z., Xiao, C., Song, D., Li, B. "AgentPoison: Red-teaming LLM Agents
via Poisoning Memory or Knowledge Bases." *NeurIPS*, 2024.
<https://arxiv.org/abs/2407.12784>

- **Priority:** should-read
- **Informs:** `read.synthesize`, `read.revise`, `read.declare`, `read.recall`, `authority.resist`, `surface.learn`
- **Question:** Should provenance tier be a hard gate on recall rather than a ranking
  signal? Extraction turns ingested third-party text into durable claims that later
  ground answers, which is exactly the path trigger-optimized poisoning targets. Open
  for the decision: should a claims table declare a minimum serving taint and withhold
  claims below it? A claim's provenance tier outweighs the count of agreeing sources, since
  agreement fails once an attacker controls volume. The same concern covers distillation: a
  conclusion drawn from one reader's rows carries those rows' labels to any later
  reader. Query-only injection lives in [prompt-injection.md](./prompt-injection.md)
  under *MINJA*.

## Grounding

### Self-RAG

Asai, A., Wu, Z., Wang, Y., Sil, A., Hajishirzi, H. "Self-RAG: Learning to Retrieve,
Generate, and Critique through Self-Reflection." *ICLR*, 2024.
<https://arxiv.org/abs/2310.11511>

- **Priority:** optional
- **Informs:** `read.recall`, `surface.ground`
- **Question:** Is support for a claim binary? Self-RAG grades it three ways — fully
  supported, partially supported, no support — which backs serving a claim with a
  graded verdict on its evidence rather than hiding it. It says nothing on how much
  evidence a claim may carry.

### ALCE

Gao, T., Yen, H., Yu, J., Chen, D. "Enabling Large Language Models to Generate Text
with Citations." *EMNLP*, 2023. <https://arxiv.org/abs/2305.14627>

- **Priority:** should-read
- **Informs:** `read.recall`, `surface.ground`, `assurance.evaluate`
- **Question:** How is a citation judged to support a statement? ALCE scores citation
  recall and precision by NLI entailment between the cited passages and the
  statement, a per-claim measure that fits claims citing evidence rows better than
  answer-level scores. It does not address capping the evidence a claim serves.
