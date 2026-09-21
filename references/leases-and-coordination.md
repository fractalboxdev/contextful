# Leases and coordination

Who may write, for how long, and how a late holder is stopped: the bucket lease,
the compaction owner, backfill chunk leases, the cadence lease and worker
heartbeats. The executable model of the lease + CAS + fence protocol lives in
[formal-methods.md](./formal-methods.md) under *How Amazon Web Services Uses Formal
Methods*.

### Leases

Gray, C. G., Cheriton, D. R. "Leases: An Efficient Fault-Tolerant Mechanism for
Distributed File Cache Consistency." *SOSP*, pp. 202–210, 1989.
<https://dl.acm.org/doi/10.1145/74850.74870>

- **Priority:** must-read
- **Informs:** `store.lease`, `topology.coordinate`, `surface.dispatch`, `run.backfill`
- **Question:** What clock-drift and process-pause bound makes a given TTL and
  renewal interval safe? A lease is correct only under bounded drift between holder
  and arbiter; with no stated skew bound, the fence becomes the only safety net, and
  every lease in the design — bucket, chunk, cadence — needs its TTL, renewal and
  skew limit stated together.

### The Chubby lock service

Burrows, M. "The Chubby lock service for loosely-coupled distributed systems."
*OSDI*, 2006.
<https://www.usenix.org/legacy/events/osdi06/tech/full_papers/burrows/burrows.pdf>

- **Priority:** must-read
- **Informs:** `store.lease`, `store.push`, `surface.dispatch`
- **Question:** Where must a sequencer be validated? Chubby's answer is the
  downstream server, not the client. A writer that compares its fence before
  committing lets a paused holder commit after its successor; the refusal is
  enforceable only if the bucket-side commit is conditional on the lease object's
  version (`If-Match` on the ETag held since acquisition), and the fence stays
  monotonic only if release keeps the object and clears the holder rather than
  deleting it.

### How to do distributed locking

Kleppmann, M. "How to do distributed locking." 2016.
<https://martin.kleppmann.com/2016/02/08/how-to-do-distributed-locking.html>

- **Priority:** should-read
- **Informs:** `store.lease`, `store.fold`, `run.backfill`, `surface.dispatch`
- **Question:** Why does a lock for correctness need a fencing token checked by the
  storage resource? The GC-pause timeline is the failure the compaction owner,
  chunk lease and cadence lease each face; the fix is the same in each — stamp the
  fence into the committed object and let the store reject a lower one.

### Borg, Omega, and Kubernetes

Burns, B., Grant, B., Oppenheimer, D., Brewer, E., Wilkes, J. "Borg, Omega, and
Kubernetes." *ACM Queue* 14(1), 2016. <https://queue.acm.org/detail.cfm?id=2898444>

- **Priority:** optional
- **Informs:** `surface.reconcile`, `surface.apply`
- **Question:** What does a level-triggered reconciler over declarative desired state
  guarantee that an edge-triggered one does not? The published basis for the pure
  schedule diff and the fail-static rule, where the last valid configuration keeps
  serving when a new one fails validation.
