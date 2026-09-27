// Verbatim from plans/2026-09-25-pokemon-trace-demo.md. Field names match the Rust serde structs.
export type Row = number[];
export type Change = { row: Row; w: number };

export type Trace = {
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

export type TraceNode = Trace["nodes"][number];
export type TraceRelation = Trace["relations"][number];
export type TraceStep = Trace["steps"][number];
export type StepNode = TraceStep["nodes"][number];
export type RelChange = TraceStep["frontier"][number];
