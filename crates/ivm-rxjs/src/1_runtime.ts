// Shared Z-set and dictionary primitives. The emitter supplies every IR operator.
type Row = number[];
type Change = { rel: number; row: Row; w: number };
type Frontier = { changes: Change[] };
type Bag = Map<string, { row: Row; w: number }>;
type Ty = 'Int' | 'Id';
type Term = { functor: string; args: Row; types: Ty[]; text?: string };
type Terms = { next: number; byId: Record<number, Term>; byKey: Record<string, number>; byText: Record<string, number>; literals: number[] };
type Frame = { rels: Map<number, Bag>; nodes: Map<number, Bag>; terms: Terms };
type State = Frame & { outputs: Map<number, Bag>; changes: Change[] };
type NodeState = { old: Bag; now: Bag; nodes: Map<number, Bag> };
const NODE_STATE: unique symbol = Symbol('node-state');
export const dictionary: unique symbol = Symbol('dictionary');
export type Batch = Change[] & { [NODE_STATE]?: NodeState; [dictionary]?: Terms };

function empty(): Bag { return new Map(); }
function copy(bag: Bag): Bag { return new Map([...bag].map(([key, value]) => [key, { row: value.row, w: value.w }])); }
function put(bag: Bag, row: Row, w: number): void {
  if (w === 0) return;
  const key = JSON.stringify(row);
  const next = (bag.get(key)?.w ?? 0) + w;
  if (next === 0) bag.delete(key);
  else bag.set(key, { row, w: next });
}
function add(into: Bag, other: Bag): Bag {
  for (const { row, w } of other.values()) put(into, row, w);
  return into;
}
function diff(before: Bag, after: Bag): Bag {
  const out = copy(after);
  for (const { row, w } of before.values()) put(out, row, -w);
  return out;
}
function sameRels(before: Map<number, Bag>, after: Map<number, Bag>): boolean {
  if (before.size !== after.size) return false;
  for (const [id, bag] of before) {
    const next = after.get(id);
    if (!next || bag.size !== next.size) return false;
    for (const [key, value] of bag) if (next.get(key)?.w !== value.w) return false;
  }
  return true;
}
function select(row: Row, cols: number[]): Row { return cols.map(col => row[col]); }
function keyOf(row: Row, cols: number[]): string { return JSON.stringify(select(row, cols)); }
function groups(bag: Bag, cols: number[]): Map<string, Bag> {
  const out = new Map<string, Bag>();
  for (const { row, w } of bag.values()) {
    const key = keyOf(row, cols);
    let group = out.get(key);
    if (!group) { group = empty(); out.set(key, group); }
    put(group, row, w);
  }
  return out;
}
function weightByKey(bag: Bag, cols: number[]): Map<string, number> {
  const out = new Map<string, number>();
  for (const { row, w } of bag.values()) {
    const key = keyOf(row, cols);
    out.set(key, (out.get(key) ?? 0) + w);
  }
  return out;
}
function indexAdd(index: Map<string, Bag>, key: string, row: Row, w: number): Map<string, Bag> {
  const next = new Map(index);
  const group = copy(next.get(key) ?? empty());
  put(group, row, w);
  if (group.size) next.set(key, group);
  else next.delete(key);
  return next;
}
function rows(batch: Batch): Bag {
  const out = empty();
  for (const { row, w } of batch) put(out, row, w);
  return out;
}
function node(batch: Batch): NodeState { return batch[NODE_STATE]!; }
function pack(id: number, old: Bag, delta: Bag, ...children: Batch[]): Batch {
  const now = add(copy(old), delta);
  const states = new Map<number, Bag>();
  for (const child of children) for (const [nodeId, bag] of node(child).nodes) states.set(nodeId, bag);
  states.set(id, now);
  const batch = [...delta.values()].map(({ row, w }) => ({ rel: id, row, w })) as Batch;
  Object.defineProperty(batch, NODE_STATE, { value: { old, now, nodes: states } });
  return batch;
}
function oldNode(nodes: Map<number, Bag>, id: number): Bag { return nodes.get(id) ?? empty(); }
function initialTerms(texts: string[], needsEmpty: boolean): Terms {
  const terms: Terms = { next: 0, byId: {}, byKey: {}, byText: {}, literals: [] };
  if (needsEmpty) mintText(terms, '');
  terms.literals = texts.map(value => mintText(terms, value));
  return terms;
}
function mint(terms: Terms, functor: string, args: Row, types: Ty[]): number {
  const key = JSON.stringify([functor, args]);
  const old = terms.byKey[key];
  if (old !== undefined) return old;
  const id = ++terms.next;
  terms.byKey[key] = id;
  terms.byId[id] = { functor, args, types };
  return id;
}
// Interns `value` whole, as `ivm_ir::Interner::mint_text` does; `splitText` decomposes on demand.
function mintText(terms: Terms, value: string): number {
  const old = terms.byText[value];
  if (old !== undefined) return old;
  const id = ++terms.next;
  terms.byText[value] = id;
  terms.byId[id] = { functor: '', args: [], types: [], text: value };
  return id;
}
function splitText(terms: Terms, id: number): [number, number] | undefined {
  const value = terms.byId[id]?.text;
  if (!value) return undefined;
  const first = [...value][0];
  return [mintText(terms, first), mintText(terms, value.slice(first.length))];
}
function constructorBag(terms: Terms, functor: string): Bag {
  const out = empty();
  for (const [id, term] of Object.entries(terms.byId)) {
    if (term.functor === functor) put(out, [+id, ...term.args], 1);
  }
  return out;
}
function termCompare(terms: Terms, a: number, b: number): number {
  if (a === b) return 0;
  const x = terms.byId[a], y = terms.byId[b];
  if (!x || !y) return !x && !y ? a - b : x ? 1 : -1;
  if (x.text !== undefined || y.text !== undefined) {
    if (x.text === undefined) return 1;
    if (y.text === undefined) return -1;
    return x.text < y.text ? -1 : x.text > y.text ? 1 : 0;
  }
  if (x.args.length !== y.args.length) return x.args.length - y.args.length;
  if (x.functor !== y.functor) return x.functor < y.functor ? -1 : 1;
  for (let i = 0; i < x.args.length; i++) {
    const xt = x.types[i], yt = y.types[i];
    const order = xt === 'Id' && yt === 'Id' ? termCompare(terms, x.args[i], y.args[i]) : xt !== yt ? (xt === 'Int' ? -1 : 1) : x.args[i] - y.args[i];
    if (order) return order;
  }
  return 0;
}
function cmpCell(ty: Ty, a: number, b: number, terms: Terms): number {
  return ty === 'Id' ? termCompare(terms, a, b) : a - b;
}
function rank(a: Row, b: Row, order: { col: number; desc: boolean }[], types: Ty[], terms: Terms): number {
  for (const entry of order) {
    const value = cmpCell(types[entry.col], a[entry.col], b[entry.col], terms);
    if (value) return entry.desc ? -value : value;
  }
  for (let i = 0; i < a.length; i++) {
    const value = cmpCell(types[i], a[i], b[i], terms);
    if (value) return value;
  }
  return 0;
}
function sourceFrontier(previous: Map<number, Bag>, frontier: Frontier, sourceIds: number[]): Map<number, Bag> {
  const rels = new Map(previous);
  for (const { rel, row, w } of frontier.changes) {
    if (!sourceIds.includes(rel)) throw new Error(`unknown source relation ${rel}`);
    const bag = rels.get(rel) === previous.get(rel) ? copy(rels.get(rel) ?? empty()) : rels.get(rel)!;
    rels.set(rel, bag);
    const before = bag.get(JSON.stringify(row))?.w ?? 0;
    if (w > 0 && before > 0) throw new Error(`present insert into relation ${rel}`);
    if (w < 0 && before <= 0) continue;
    put(bag, row, w);
  }
  return rels;
}
