## reserve

Reserved names, and the commit protocol that orders a table's commits.

- `column-name` — A producer column inside the reserved namespace raises `StoreReservedColumnName`.
  *because a producer column inside the namespace shadows an injected one*
- `table-name` — A declared table inside the reserved namespace refuses; {{store.reserve.commit-wait}} bounds its commit.

### commit

Commits take their table's lock in sequence order.

- `commit-wait` — A commit waits for its table's lock, then raises `StoreCommitLockTimeout`.
  *A-store#lease*
- `commit-order` — Commits publish in sequence order; {{store.fold}} reads them, and `StoreReservedColumnName` refuses first.

#### Scenarios

- `store.reserve.commit-wait`: WHEN two commits contend, THEN the second waits.
