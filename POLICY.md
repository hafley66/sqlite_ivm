# Policy

sqlite_ivm exists to serve sprefa. Every rule below follows from that.

## What a view is

A materialized view is a cache of a query. Nobody writes to it. Its contents
are fully determined by the stored query and the current source rows.

Everything the extension keeps on disk (shadow tables, arrangements, the
`_state` table, triggers) is derived. None of it is user data.

## Upgrades: rebuild, never migrate

When the storage format or the trigger wire format changes, the extension
drops the derived objects and re-derives them from `__ivm_views.query_sql`.
It does not transform old state into new state.

A partially rebuilt view is not a corrupted view. It is a view that needs
rebuilding again, which is the same operation.

This deletes the entire class of migration durability problems. There is no
half-migrated state to be stuck in, because there is no state worth keeping.

Consequences accepted:

- rebuild cost is paid once per format change, per view
- a read-only database cannot rebuild, so it serves the old format or errors
- concurrent rebuild is a race between two identical outcomes

## Correctness is not negotiable

Rebuild covers durability. It does not cover a view that computes the wrong
answer. Any divergence between the incremental result and the fresh query is
a defect and stops work.

The invariant: every operator emits the row stored in its arrangement.

## Durability problems are TODO, not blockers

Anything whose worst case is "rebuild the view" is a TODO. Anything whose
worst case is "wrong rows, no error" is a blocker.

| class | disposition |
|---|---|
| wrong answer, silent | blocker |
| wrong answer, loud | blocker |
| stuck state recoverable by rebuild | TODO |
| perf | TODO unless it breaks the 10-second law |
| scope not used by sprefa | refuse at `bind` |

## Scope

sprefa is the only consumer. A construct sprefa does not use is refused at
`bind` with a named error rather than half-implemented. Re-admitting one is a
deliberate decision, not a side effect.
