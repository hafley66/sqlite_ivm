import type { Change, Row, Trace, TraceNode, TraceStep } from "./0_trace";

// ---- concept tokens (plan table "Concept ids") ----

export const rowKey = (row: Row) => row.join("-");

export const tokens = {
  entity: (value: number) => `entity:${value}`,
  rel: (name: string) => `rel:${name}`,
  node: (id: number) => `node:${id}`,
  row: (row: Row) => `row:${rowKey(row)}`,
  term: (index: number) => `term:${index}`,
  step: (index: number) => `step:${index}`,
  event: (kind: "granted" | "revoked") => `event:${kind}`,
  column: (name: string) => `column:${name.replace(/\s+/g, "_")}`,
};

export const isEntity = (names: Trace["names"], value: number) => String(value) in names;

// A row mention carries its row token, each entity token, and its column tokens.
export const rowConcept = (names: Trace["names"], row: Row, columns: string[]) =>
  [
    tokens.row(row),
    ...row.filter((value) => isEntity(names, value)).map(tokens.entity),
    ...row.map((_, index) => tokens.column(columnName(columns, index))),
  ].join(" ");

export const columnName = (columns: string[], index: number) => columns[index] ?? `value ${index + 1}`;

export type Cell =
  | { kind: "entity"; value: number; label: string }
  | { kind: "number"; value: number; column: string };

// A join row repeats its key once per side ("pokemon", "pokemon"); a repeated column with the same value is said once.
export const cells = (names: Trace["names"], row: Row, columns: string[]): Cell[] =>
  row
    .map((value, index): Cell =>
      isEntity(names, value)
        ? { kind: "entity", value, label: names[String(value)] }
        : { kind: "number", value, column: columnName(columns, index) },
    )
    .filter((_, index) => !row.some((value, earlier) => earlier < index && value === row[index] && columnName(columns, earlier) === columnName(columns, index)));

// ---- multiset arithmetic over changes ----

export const consolidate = (changes: Change[]): Change[] => {
  const sums = new Map<string, Change>();
  for (const change of changes) {
    const key = rowKey(change.row);
    const seen = sums.get(key);
    sums.set(key, { row: change.row, w: (seen?.w ?? 0) + change.w });
  }
  return [...sums.values()]
    .filter((change) => change.w !== 0)
    .sort((left, right) => compareRows(left.row, right.row));
};

export const compareRows = (left: Row, right: Row) => {
  for (let index = 0; index < Math.max(left.length, right.length); index++) {
    const diff = (left[index] ?? 0) - (right[index] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
};

export const sameChanges = (left: Change[], right: Change[]) => {
  const a = consolidate(left);
  const b = consolidate(right);
  return a.length === b.length && a.every((change, index) => rowKey(change.row) === rowKey(b[index].row) && change.w === b[index].w);
};

export const weightOf = (changes: Change[], row: Row) =>
  changes.filter((change) => rowKey(change.row) === rowKey(row)).reduce((sum, change) => sum + change.w, 0);

export const forRelation = (changes: TraceStep["frontier"], relation: number): Change[] =>
  changes.filter((change) => change.relation === relation).map(({ row, w }) => ({ row, w }));

// ---- words for the learner ----

const opWords: Record<string, string> = {
  Get: "read",
  Mfp: "reshape",
  Union: "combine",
  Negate: "flip signs",
  Join: "match up",
  Antijoin: "keep unmatched",
  Reduce: "summarize",
  Threshold: "keep if any reason",
  TopK: "pick the top",
};

export const opWord = (op: string) => opWords[op] ?? op.toLowerCase();

export const relationOf = (trace: Trace, id: number | null) =>
  id === null ? undefined : trace.relations.find((relation) => relation.id === id);

export const nodeTitle = (trace: Trace, node: TraceNode) =>
  node.caption || relationOf(trace, node.relation)?.name || opWord(node.op);

export const nodeConcept = (trace: Trace, node: TraceNode) =>
  [tokens.node(node.id), ...(relationOf(trace, node.relation) ? [tokens.rel(relationOf(trace, node.relation)!.name)] : [])].join(" ");

export const reasons = (count: number) => `${count} ${Math.abs(count) === 1 ? "reason" : "reasons"}`;

// ---- cast ----

export const castGroups = (names: Trace["names"]) => {
  const groups = [
    { title: "Trainers", test: (value: number) => value >= 1001 && value < 2000 },
    { title: "Pokémon", test: (value: number) => value >= 2000 && value < 3000 },
    { title: "Moves", test: (value: number) => value >= 3000 && value <= 4000 },
    { title: "Places", test: (value: number) => value >= 4001 },
    { title: "Other", test: (value: number) => value < 1001 },
  ];
  const values = Object.keys(names).map(Number).sort((a, b) => a - b);
  return groups
    .map((group) => ({ title: group.title, values: values.filter(group.test) }))
    .filter((group) => group.values.length > 0);
};

// `-- expect-error:` steps: the engines refuse the step and nothing changes.
export const errorWords = (error: string) =>
  error.includes("PresentInsert") ? "that row is already there, so adding it again is refused" : error;
