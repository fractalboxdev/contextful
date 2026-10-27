---
contract: store
owns:
  - fold
---

# The store and its sync

## fold

Compaction: pass order, triggers, retention, and the pointer commit that makes a snapshot readable.

- `shape-keys` — A shape block takes {{type:contextful_demo::shape::Shape}}.
- `fill-modes` — A shape fills gaps by {{type:contextful_demo::shape::Fill}}.
