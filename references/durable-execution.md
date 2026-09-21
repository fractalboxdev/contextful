# Durable execution

Journaled runs that replay after a crash, cursors that advance after the write,
incremental windows over late data, and retries against shared vendor quotas.

### Durable Functions

Burckhardt, S., Gillum, C., Justo, D., Kallas, K., McMahon, C., Meiklejohn, C. S.
"Durable Functions: Semantics for Stateful Serverless." *OOPSLA* (PACMPL 5), 2021.
<https://dl.acm.org/doi/10.1145/3485510>

- **Priority:** must-read
- **Informs:** `run.journal`, `run.suspend`, `surface.dispatch`, `surface.fire`
- **Question:** What replay semantics does the journal's effect boundary promise? A
  body replays faithfully exactly when every side effect passes through a recorded
  step; the paper formalizes that and proves two execution models equivalent, which
  makes it the reference-model target for `run` beside the authority model.

### Beldi

Zhang, H., Cardoza, A., Chen, P. B., Angel, S., Liu, V. "Fault-tolerant and
Transactional Stateful Serverless Workflows." *OSDI*, 2020.
<https://www.usenix.org/conference/osdi20/presentation/zhang-haoran>

- **Priority:** should-read
- **Informs:** `run.journal`, `run.own`, `surface.dispatch`, `connector.meter`
- **Question:** Exactly-once effects or exactly-once records? Under insert-or-ignore
  admission both racers compute and one row survives, so the value is recorded once
  while the effect runs at least once — two billed calls, two meter permits. Beldi's
  intent log shows the two ways out: an idempotency key per outbound request, or
  claiming a pending row before the effect runs.

### State Management in Apache Flink

Carbone, P., Ewen, S., Fóra, G., Haridi, S., Richter, S., Tzoumas, K. "State
Management in Apache Flink: Consistent Stateful Distributed Stream Processing."
*PVLDB* 10(12):1718–1729, 2017. <https://www.vldb.org/pvldb/vol10/p1718-carbone.pdf>

- **Priority:** should-read
- **Informs:** `run.advance`, `run.land`, `run.own`
- **Question:** How does a sink commit atomically with its source position? A
  snapshot committed in Parquet and a cursor committed in SQLite have no shared
  boundary; checkpoint barriers and two-phase sink commit are the published
  mechanism, and the Delta log's `txn` action (see
  [storage-and-table-formats.md](./storage-and-table-formats.md)) is the
  object-store form of it.

### The Dataflow Model

Akidau, T., et al. "The Dataflow Model: A Practical Approach to Balancing
Correctness, Latency, and Cost in Massive-Scale, Unbounded, Out-of-Order Data
Processing." *PVLDB* 8(12):1792–1803, 2015.
<https://www.vldb.org/pvldb/vol8/p1792-Akidau.pdf>

- **Priority:** should-read
- **Informs:** `run.advance`, `run.backfill`, `connector.source`
- **Question:** What allowed lateness does an incremental source accept, and how is a
  restated window retracted? Event time versus processing time, watermarks and
  allowed lateness settle the inclusive-boundary and lookback questions the source
  cursors leave open.

### Metastable Failures in Distributed Systems

Bronson, N., Aghayev, A., Charapko, A., Zhu, T. "Metastable Failures in Distributed
Systems." *HotOS*, 2021.
<https://sigops.org/s/conferences/hotos/2021/papers/hotos21-s11-bronson.pdf>

- **Priority:** should-read
- **Informs:** `run.retry`, `connector.meter`, `connector.lease`, `run.select`
- **Question:** Which layer owns retry? Nested retry layers multiply attempts, a
  verbatim `Retry-After: 86400` holds a run slot for a day, and zero jitter
  synchronizes clients — each a documented trigger of sustained overload against a
  shared quota. The paper argues for one retrying layer, a clamped server hint and
  non-zero jitter.

## Standards

### JSON Canonicalization Scheme

Rundgren, A., Jordan, B., Erdtman, S. "JSON Canonicalization Scheme (JCS)."
RFC 8785, 2020. <https://www.rfc-editor.org/rfc/rfc8785>

- **Priority:** should-read
- **Informs:** `run.declare`
- **Question:** What does "canonical JSON" mean for the pipeline declaration's
  content hash? RFC 8785 fixes key order, number serialization and string escaping;
  the declaration still has to say whether an explicit default hashes equal to its
  absence.
