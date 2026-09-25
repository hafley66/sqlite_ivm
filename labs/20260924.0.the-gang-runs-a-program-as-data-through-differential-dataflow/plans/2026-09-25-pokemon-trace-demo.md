# Pokémon trace demo

One HTML page teaches how the program runs on both engines. Every value on the page comes from
`examples/1_trace.rs`, which runs a real oracle script through `Dd` and `Sql` and writes JSON.
The page contains no hand-typed data.

## Parts and owners

| part | files | owner |
|---|---|---|
| engine observation hook | `src/1_rel.rs`, `src/2_dd.rs`, `src/3_sqlite.rs` | rust lane |
| harness step reader | `tests/support/mod.rs` | rust lane |
| trace writer | `examples/1_trace.rs` | rust lane |
| Pokémon scripts | `oracle/pokemon/*.sql`, `oracle/pokemon/*.program.json`, test registrations in `tests/0_scripts.rs`, `tests/1_sqlite_scripts.rs` | scripts lane |
| page | `demo/**` | page lane |

## Script header (read by the trace writer; SQLite ignores the comments)

```sql
-- title: Can Red Surf?
-- question: Which moves can each trainer use outside battle?
-- name: 1001 Red
-- name: 2131 Lapras
-- name: 3057 Surf
-- node: 3 party members matched to the moves they know
-- node: 6 can use: yes or no
CREATE TABLE party(trainer INTEGER NOT NULL, pokemon INTEGER NOT NULL, PRIMARY KEY(trainer, pokemon));
...
-- step: Red catches Lapras and Starmie
```

- `-- name: <value> <label>`: display label for a cell value. Labels are global per script.
  Entity ids are chosen so they never collide with plain numbers such as levels:
  trainers 1001+, Pokémon 2000 + National Dex number, moves 3000 + move id, towns and routes 4001+.
- `-- node: <ir node id> <caption>`: plain-language caption for an IR node.
- `-- step: <sentence>`: the step name is the caption shown on the page.

## Observation hook

`Rel` gains one default method, called by `lower_node` on every node it builds, including nodes
built inside a LetRec:

```rust
fn observe(&mut self, _id: NodeId, c: Self::C) -> Self::C { c }
```

- Engines built by `install` keep the default. Operator counts, gates and benchmarks do not change.
- `Dd` traced mode: `observe` attaches `inspect` and records `(node, row, time, w)`.
  Times inside a LetRec record the loop round.
- `Sql` traced mode: `observe` records which SQL nodes belong to the IR node. The returned SQL node
  is the node's own delta; SQL nodes pushed since the children returned are its internal nodes.
  Join nodes also keep the SQL of each of their three delta terms, so the trace can count what each
  term contributed.

## Trace JSON (`demo/traces/<scenario>.json`)

Field names are identical in the Rust serde structs and in `demo/src/0_trace.ts`.

```ts
type Row = number[];
type Change = { row: Row; w: number };

type Trace = {
  scenario: string;                 // "0_can_surf"
  title: string;
  question: string;
  names: Record<string, string>;    // cell value (decimal string) -> label
  relations: { id: number; name: string; kind: "Source" | "Derived"; columns: string[] }[];
  nodes: {
    id: number;                     // IR node id
    op: string;                     // "Get" | "Mfp" | "Union" | "Negate" | "Join" | "Antijoin" | "Reduce" | "Threshold" | "TopK"
    detail: unknown;                // the IR op as serialized by serde
    inputs: number[];               // IR node ids read by this node
    relation: number | null;        // Get: relation read; the stratum body: relation defined
    caption: string;                // from `-- node:`; "" when absent
    columns: string[];              // column names propagated from source columns
    in_loop: boolean;               // built inside a LetRec
    sqlite_tables: string[];        // own delta table first, then internal ones
    sqlite_fill: string[];          // generated SQL, same order
  }[];
  steps: {
    index: number;
    caption: string;
    frontier: { relation: number; row: Row; w: number }[];
    error: string | null;           // `-- expect-error:` steps
    oracle: { relation: number; row: Row; w: number }[];  // SQLite view diff, the expected answer
    dd: { relation: number; row: Row; w: number }[];      // Delta from Dd
    sqlite: { relation: number; row: Row; w: number }[];  // Delta from Sql
    nodes: {
      id: number;
      dd_changes: Change[];                               // consolidated for this step
      dd_rounds: { round: number; row: Row; w: number }[]; // loop nodes only
      sqlite_changes: Change[];                            // the node's delta table, consolidated
      totals_before: Change[];                             // the node's running-totals table before this step; [] when the node keeps none
      terms: { label: string; changes: Change[] }[];       // Join only
    }[];
  }[];
};
```

Join term labels, for inputs named A and B by their captions:
`"new A × new B"`, `"existing A × new B"`, `"new A × existing B"`.

## Concept ids (page)

Every rendered mention carries `data-concept` with space-separated tokens. Hovering any mention lights
every element that shares a token, including Cytoscape nodes and edges.

| token | example |
|---|---|
| `entity:<value>` | `entity:1001` |
| `rel:<name>` | `rel:party` |
| `node:<id>` | `node:3` |
| `row:<v>-<v>...` | `row:1001-3057` |
| `term:<index>` | `term:0` |
| `step:<index>` | `step:2` |
| `event:granted` / `event:revoked` | a zero crossing on a Threshold node |
| `column:<name>` | `column:trainer` |

## Scenarios

| file | original | setting |
|---|---|---|
| `0_can_surf` | `0_access` | party ⋈ knows ∪ ride_pager: which field moves a trainer can use |
| `1_party_stats` | `1_team_cost` | party_slot(pokemon, trainer, level): count, total, min, max level |
| `2_party_size` | `10_team_sum` | the same table: count and total level |
| `3_rematch` | `4_antijoin` | trainers on routes past 10 who have no win recorded against them |
| `4_two_roads` | `5_self_join` | towns exactly two roads apart |
| `5_leads` | `6_topk` | lead Pokémon and Double Battle pair per trainer |
| `6_walk` | `7_reach` | where a trainer can walk; Snorlax blocks a town |
| `7_rare_candy` | `9_depth_cap` | levels passed from a start level up to the cap |

Each scenario translates the original steps one for one. Row shapes, shared keys, multiplicities and
cycles stay identical to the original. Only names and values change.

## Build

`just -f` recipe in the lab: run `1_trace` for every `oracle/pokemon/*.sql`, then build `demo/` with
Vite into one self-contained `demo/dist/index.html` (React, Tailwind, Cytoscape, vite-plugin-singlefile).
Traces are generated, not committed.
