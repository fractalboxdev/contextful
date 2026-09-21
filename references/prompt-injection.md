# Prompt injection

Third-party text reaches a model through connector inference, the console's store
overlay and its web supplement. Enforcement sits below the prompt; these sources
state how much the prompt-level defenses leave over. Memory poisoning lives in
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
