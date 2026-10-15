# Prompt injection

Third-party text reaches a model through connector inference, the console's store
overlay and its web supplement. Enforcement sits below the prompt; these sources
state how much the prompt-level defenses leave over. *MINJA* and *PoisonedRAG*
cover injection into memory and the retrieval corpus through ordinary writes;
trigger-optimized memory poisoning lives in
[retrieval-and-memory.md](./retrieval-and-memory.md) under *AgentPoison*.

### Spotlighting

Hines, K., Lopez, G., Hall, M., Zarfati, F., Zunger, Y., Kiciman, E. "Defending
Against Indirect Prompt Injection Attacks With Spotlighting." arXiv:2403.14720,
2024. <https://arxiv.org/abs/2403.14720>

- **Priority:** must-read
- **Informs:** `connector.infer`, `authority.resist`, `surface.plan-turn`
- **Question:** What residue does the data fence leave? The fence and marker
  derivation are spotlighting (delimiting and datamarking), which cuts attack success
  from over 50% to under 2%, not to zero. The fence is therefore not a security
  boundary, and model output that re-lands as data inherits the least-trusted label
  among its inputs.

### CaMeL and FIDES

Debenedetti, E., et al. "Defeating Prompt Injections by Design." arXiv:2503.18813,
2025. <https://arxiv.org/abs/2503.18813>

Costa, M., et al. "Securing AI Agents with Information-Flow Control."
arXiv:2505.23643, 2025. <https://arxiv.org/abs/2505.23643>

- **Priority:** should-read
- **Informs:** `authority.resist`, `surface.ground`, `surface.plan-turn`
- **Question:** Which injection harms does the design eliminate and which does it only
  bound? Deterministic separation of control flow from data flow, with capability
  labels on values, gives the vocabulary for that statement, and is the route if the
  console's closed, read-only tool set ever gains a write tool.

### Not What You've Signed Up For

Greshake, K., Abdelnabi, S., Mishra, S., Endres, C., Holz, T., Fritz, M. "Not What
You've Signed Up For: Compromising Real-World LLM-Integrated Applications with
Indirect Prompt Injection." *AISec*, 2023. <https://arxiv.org/abs/2302.12173>

- **Priority:** optional
- **Informs:** `authority.resist`, `connector.infer`, `surface.ground`
- **Question:** Which threats does resistance defend against? The taxonomy — data
  theft, worming, ecosystem contamination — replaces restated threat prose with one
  named model.

### MINJA

Dong, S., Xu, S., He, P., Li, Y., Tang, J., Liu, T., Liu, H., Xiang, Z. "Memory
Injection Attacks on LLM Agents via Query-Only Interaction." arXiv:2503.03704, 2025.
<https://arxiv.org/abs/2503.03704>

- **Priority:** must-read
- **Informs:** `read.revise`, `read.declare`, `read.recall`, `read.synthesize`
- **Question:** Can an attacker with no write access poison memory? MINJA plants
  malicious records through ordinary queries and observed outputs alone, at a 98.2%
  average injection success rate. The attack settles that a write a model chose
  stands below `curated`: an agent member on the credential, or third-party taint on
  a cited row, stamps `derived`, and a claims table's minimum serving taint withholds
  what falls below it. Provenance tier, not source count, decides what grounds an
  answer; what binds an asserted agent member to the writer stays open.

### PoisonedRAG

Zou, W., Geng, R., Wang, B., Jia, J. "PoisonedRAG: Knowledge Corruption Attacks to
Retrieval-Augmented Generation of Large Language Models." *USENIX Security*, 2025.
<https://arxiv.org/abs/2402.07867>

- **Priority:** should-read
- **Informs:** `read.declare`, `read.revise`, `read.retrieve`, `authority.resist`
- **Question:** How few injected texts corrupt a retrieval corpus? Five texts per
  target question reach a 90% attack success rate in a knowledge database of millions
  of texts. Rank cannot separate them from honest rows, so third-party taint follows
  every claim extracted from such rows, and a claims table declares the taint floor
  it serves.
