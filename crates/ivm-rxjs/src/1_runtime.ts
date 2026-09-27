// This source is embedded verbatim after the generated `program` constant.
type Row = number[];
type Change = { rel: number; row: Row; w: number };
type Frontier = { changes: Change[] };
type Bag = Map<string, { row: Row; w: number }>;
type Ty = 'Int' | 'Id';
type Relation = { id: number; name: string; cols: Ty[]; kind: 'Source' | 'Derived' | 'Constructor' };
type Expr = { Col: number } | { Lit: number } | { Text: number } | { Call: [string, Expr[]] };
type Order = { col: number; desc: boolean };
type Agg = 'Count' | { Sum: number } | { Min: number } | { Max: number };
type WinFn = 'RowNumber' | 'Rank' | 'DenseRank' | 'Count' | { Lag: number } | { Lead: number } | { Sum: number };
type Op =
  | { Get: number } | { Mint: { input: number; functor: number; args: number[] } }
  | { StrCons: { input: number; mode: { Construct: { head: number; rest: number } } | { Decompose: { whole: number } } } }
  | { Mfp: { input: number; filter?: Expr[]; map?: Expr[]; project?: number[] } }
  | { Union: number[] } | { Negate: number }
  | { Join: { inputs: number[]; equivalences: [number, number][][] } }
  | { Antijoin: { l: number; r: number; lk: number[]; rk: number[] } }
  | { Reduce: { input: number; key: number[]; aggs: Agg[] } }
  | { Threshold: number }
  | { TopK: { input: number; key: number[]; order: Order[]; limit: number } }
  | { Window: { input: number; partition: number[]; order: Order[]; func: WinFn } }
  | { Delay: number };
type Rec = { ids: number[]; bodies: number[]; limit: number | null };
type Stratum = { Let: { id: number; body: number } } | { LetRec: Rec };
type Program = { texts: string[]; rels: Relation[]; nodes: Op[]; strata: Stratum[]; outputs: number[] };
type Term = { functor: string; args: Row; types: Ty[]; text?: string; split?: [number, number] };
type Terms = { next: number; byId: Record<number, Term>; byKey: Record<string, number>; byText: Record<string, number>; literals: number[] };
type State = { rels: Map<number, Bag>; nodes: Map<number, Bag>; outputs: Map<number, Bag>; terms: Terms; tick: number; changes: Change[] };

export const dictionary: unique symbol = Symbol('dictionary');
export type DeltaBatch = Change[] & { [dictionary]?: Terms };

function empty(): Bag { return new Map(); }
function copy(bag: Bag): Bag { return new Map([...bag].map(([k, v]) => [k, { row: v.row, w: v.w }])); }
function put(bag: Bag, row: Row, w: number): void {
  if (w === 0) return;
  const key = JSON.stringify(row);
  const next = (bag.get(key)?.w ?? 0) + w;
  if (next === 0) bag.delete(key);
  else bag.set(key, { row, w: next });
}
function entries(bag: Bag): { row: Row; w: number }[] { return [...bag.values()]; }
function same(a: Bag, b: Bag): boolean {
  return a.size === b.size && [...a].every(([k, v]) => b.get(k)?.w === v.w);
}
function diff(a: Bag, b: Bag): Bag {
  const out = copy(b);
  for (const { row, w } of a.values()) put(out, row, -w);
  return out;
}
function select(row: Row, cols: number[]): Row { return cols.map(c => row[c]); }
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
function mint(terms: Terms, functor: string, args: Row, types: Ty[]): number {
  const key = JSON.stringify([functor, args]);
  const old = terms.byKey[key];
  if (old !== undefined) return old;
  const id = ++terms.next;
  terms.byKey[key] = id;
  terms.byId[id] = { functor, args, types };
  return id;
}
function mintText(terms: Terms, value: string): number {
  const old = terms.byText[value];
  if (old !== undefined) return old;
  let split: [number, number] | undefined;
  if (value.length) {
    const first = [...value][0];
    const rest = value.slice(first.length);
    const restId = mintText(terms, rest);
    split = rest.length ? [mintText(terms, first), restId] : [0, restId];
  }
  const id = ++terms.next;
  if (split?.[0] === 0) split[0] = id;
  terms.byText[value] = id;
  terms.byId[id] = { functor: '', args: [], types: [], text: value, split };
  return id;
}
function initialTerms(): Terms {
  const terms: Terms = { next: 0, byId: {}, byKey: {}, byText: {}, literals: [] };
  if (program.texts.length || program.nodes.some(op => 'StrCons' in op || ('Mfp' in op && JSON.stringify(op).includes('StrNil')))) mintText(terms, '');
  terms.literals = program.texts.map(text => mintText(terms, text));
  return terms;
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
function evalExpr(expr: Expr, row: Row, terms: Terms): number {
  if ('Col' in expr) return row[expr.Col];
  if ('Lit' in expr) return expr.Lit;
  if ('Text' in expr) return terms.literals[expr.Text];
  const [func, args] = expr.Call;
  const a = () => evalExpr(args[0], row, terms), b = () => evalExpr(args[1], row, terms);
  switch (func) {
    case 'Eq': return +(a() === b()); case 'Ne': return +(a() !== b());
    case 'Lt': return +(a() < b()); case 'Le': return +(a() <= b());
    case 'Gt': return +(a() > b()); case 'Ge': return +(a() >= b());
    case 'Add': return Number(BigInt.asIntN(64, BigInt(a()) + BigInt(b())));
    case 'Sub': return Number(BigInt.asIntN(64, BigInt(a()) - BigInt(b())));
    case 'And': return +(a() !== 0 && b() !== 0);
    case 'Or': return +(a() !== 0 || b() !== 0);
    case 'Not': return +(a() === 0);
    case 'TermLt': return +(termCompare(terms, a(), b()) < 0);
    case 'StrNil': return terms.byText[''];
    default: throw new Error(`unknown function ${func}`);
  }
}
function typesOf(id: number): Ty[] {
  const op = program.nodes[id];
  if ('Get' in op) return program.rels.find(r => r.id === op.Get)!.cols;
  if ('Mint' in op) return [...typesOf(op.Mint.input), 'Id'];
  if ('StrCons' in op) return [...typesOf(op.StrCons.input), ...('Construct' in op.StrCons.mode ? ['Id' as Ty] : ['Id' as Ty, 'Id' as Ty])];
  if ('Mfp' in op) {
    const cols = [...typesOf(op.Mfp.input), ...(op.Mfp.map ?? []).map(e => ('Text' in e || ('Call' in e && e.Call[0] === 'StrNil') ? 'Id' : 'Int') as Ty)];
    return op.Mfp.project?.length ? op.Mfp.project.map(c => cols[c]) : cols;
  }
  if ('Union' in op) return typesOf(op.Union[0]);
  if ('Negate' in op) return typesOf(op.Negate);
  if ('Join' in op) return op.Join.inputs.flatMap(typesOf);
  if ('Antijoin' in op) return typesOf(op.Antijoin.l);
  if ('Reduce' in op) return [...op.Reduce.key.map(c => typesOf(op.Reduce.input)[c]), ...op.Reduce.aggs.map(a => typeof a === 'string' || 'Sum' in a ? 'Int' : typesOf(op.Reduce.input)['Min' in a ? a.Min : a.Max])];
  if ('Threshold' in op) return typesOf(op.Threshold);
  if ('TopK' in op) return typesOf(op.TopK.input);
  if ('Window' in op) return [...typesOf(op.Window.input), 'Int'];
  throw new Error('Delay unsupported');
}
function rank(a: Row, b: Row, order: Order[], types: Ty[], terms: Terms): number {
  const cmp = (col: number) => types[col] === 'Id' ? termCompare(terms, a[col], b[col]) : a[col] - b[col];
  for (const o of order) { const v = cmp(o.col); if (v) return o.desc ? -v : v; }
  for (let i = 0; i < a.length; i++) { const v = cmp(i); if (v) return v; }
  return 0;
}

type NodeStep = { old: Bag; now: Bag; delta: Bag };
function joinBags(left: Bag, right: Bag, lk: number[], rk: number[]): Bag {
  const out = empty();
  const index = groups(right, rk);
  for (const l of left.values()) for (const r of index.get(keyOf(l.row, lk))?.values() ?? []) {
    put(out, [...l.row, ...r.row], l.w * r.w);
  }
  return out;
}

/** Each node contributes one Observable of its frontier delta and retained Z-set. */
function advanceNode$(
  id: number, oldRels: Map<number, Bag>, rels: Map<number, Bag>, oldNodes: Map<number, Bag>,
  nodes: Map<number, Bag>, terms: Terms, streams: Map<number, import('rxjs').Observable<NodeStep>>,
): import('rxjs').Observable<NodeStep> {
  const existing = streams.get(id);
  if (existing) return existing;
  const old = oldNodes.get(id) ?? empty();
  const cached = nodes.get(id);
  if (cached) return of({ old, now: cached, delta: diff(old, cached) });
  const op = program.nodes[id];
  const sub = (node: number) => advanceNode$(node, oldRels, rels, oldNodes, nodes, terms, streams);
  const finish = (delta: Bag): NodeStep => {
    const now = copy(old);
    for (const { row, w } of delta.values()) put(now, row, w);
    nodes.set(id, now);
    return { old, now, delta };
  };
  let result$: import('rxjs').Observable<NodeStep>;
  if ('Get' in op) {
    result$ = defer(() => {
      const rel = program.rels.find(r => r.id === op.Get)!;
      const now = rel.kind === 'Constructor' ? evaluate(id, rels, terms, new Map()) : rels.get(op.Get) ?? empty();
      nodes.set(id, now);
      return of({ old, now, delta: diff(old, now) });
    });
  } else if ('Mint' in op || 'StrCons' in op || 'Mfp' in op || 'Negate' in op) {
    const input = 'Negate' in op ? op.Negate : 'Mint' in op ? op.Mint.input : 'StrCons' in op ? op.StrCons.input : op.Mfp.input;
    result$ = sub(input).pipe(map(child => finish(evaluate(id, rels, terms, new Map([[input, child.delta]])))));
  } else if ('Union' in op) {
    result$ = merge(...op.Union.map(sub)).pipe(
      scan((delta, child) => {
        const next = copy(delta);
        for (const { row, w } of child.delta.values()) put(next, row, w);
        return next;
      }, empty()),
      last(), map(finish),
    );
  } else if ('Join' in op) {
    const [left, right] = op.Join.inputs;
    const lk = op.Join.equivalences.map(eq => eq.find(pair => pair[0] === 0)![1]);
    const rk = op.Join.equivalences.map(eq => eq.find(pair => pair[0] === 1)![1]);
    result$ = forkJoin([sub(left), sub(right)]).pipe(
      mergeMap(([a, b]) => merge(
        of(joinBags(a.delta, b.old, lk, rk)),
        of(joinBags(a.old, b.delta, lk, rk)),
        of(joinBags(a.delta, b.delta, lk, rk)),
      ).pipe(
        scan((delta, bag) => {
          const next = copy(delta);
          for (const { row, w } of bag.values()) put(next, row, w);
          return next;
        }, empty()),
        last(),
      )),
      map(finish),
    );
  } else if ('Antijoin' in op) {
    const { l, r, lk, rk } = op.Antijoin;
    result$ = forkJoin([sub(l), sub(r)]).pipe(map(([left, right]) => {
      const delta = empty();
      const oldCounts = new Map<string, number>(), newCounts = new Map<string, number>();
      for (const { row, w } of right.old.values()) oldCounts.set(keyOf(row, rk), (oldCounts.get(keyOf(row, rk)) ?? 0) + w);
      for (const { row, w } of right.now.values()) newCounts.set(keyOf(row, rk), (newCounts.get(keyOf(row, rk)) ?? 0) + w);
      for (const { row, w } of left.delta.values()) if ((newCounts.get(keyOf(row, lk)) ?? 0) <= 0) put(delta, row, w);
      for (const { row, w } of left.old.values()) {
        const key = keyOf(row, lk);
        const before = (oldCounts.get(key) ?? 0) <= 0;
        const after = (newCounts.get(key) ?? 0) <= 0;
        if (before !== after) put(delta, row, after ? w : -w);
      }
      return finish(delta);
    }));
  } else if ('Threshold' in op) {
    result$ = sub(op.Threshold).pipe(map(input => {
      const delta = empty();
      for (const { row } of input.delta.values()) {
        const key = JSON.stringify(row);
        const before = (input.old.get(key)?.w ?? 0) > 0;
        const after = (input.now.get(key)?.w ?? 0) > 0;
        if (before !== after) put(delta, row, after ? 1 : -1);
      }
      return finish(delta);
    }));
  } else if ('Reduce' in op || 'TopK' in op || 'Window' in op) {
    const input = 'Reduce' in op ? op.Reduce.input : 'TopK' in op ? op.TopK.input : op.Window.input;
    const key = 'Reduce' in op ? op.Reduce.key : 'TopK' in op ? op.TopK.key : op.Window.partition;
    result$ = sub(input).pipe(map(child => {
      const delta = empty();
      const before = groups(child.old, key), after = groups(child.now, key);
      const touched = new Set(entries(child.delta).map(({ row }) => keyOf(row, key)));
      for (const group of touched) {
        const oldResult = evaluate(id, oldRels, terms, new Map([[input, before.get(group) ?? empty()]]));
        const newResult = evaluate(id, rels, terms, new Map([[input, after.get(group) ?? empty()]]));
        for (const { row, w } of diff(oldResult, newResult).values()) put(delta, row, w);
      }
      return finish(delta);
    }));
  } else throw new Error('Delay unsupported');
  const shared = result$.pipe(shareReplay({ bufferSize: 1, refCount: false }));
  streams.set(id, shared);
  return shared;
}

function evaluate(id: number, rels: Map<number, Bag>, terms: Terms, cache: Map<number, Bag>): Bag {
  const cached = cache.get(id);
  if (cached) return cached;
  const op = program.nodes[id];
  const sub = (node: number) => evaluate(node, rels, terms, cache);
  const out = empty();
  if ('Get' in op) {
    const rel = program.rels.find(r => r.id === op.Get)!;
    if (rel.kind === 'Constructor') {
      for (const [rawId, term] of Object.entries(terms.byId)) {
        if (term.functor === rel.name) put(out, [+rawId, ...term.args], 1);
      }
    } else for (const { row, w } of rels.get(op.Get)?.values() ?? []) put(out, row, w);
  } else if ('Mint' in op) {
    const { input, functor, args } = op.Mint;
    const rel = program.rels.find(r => r.id === functor)!;
    for (const { row, w } of sub(input).values()) {
      const id = mint(terms, rel.name, select(row, args), rel.cols.slice(1));
      put(out, [...row, id], w);
    }
  } else if ('StrCons' in op) {
    for (const { row, w } of sub(op.StrCons.input).values()) {
      const mode = op.StrCons.mode;
      if ('Construct' in mode) {
        const head = terms.byId[row[mode.Construct.head]]?.text;
        const rest = terms.byId[row[mode.Construct.rest]]?.text;
        if (head !== undefined && rest !== undefined) put(out, [...row, mintText(terms, head + rest)], w);
      } else {
        const split = terms.byId[row[mode.Decompose.whole]]?.split;
        if (split) put(out, [...row, ...split], w);
      }
    }
  } else if ('Mfp' in op) {
    const { input, filter = [], map: maps = [], project = [] } = op.Mfp;
    for (const { row, w } of sub(input).values()) {
      if (!filter.every(e => evalExpr(e, row, terms) !== 0)) continue;
      const next = [...row];
      for (const e of maps) next.push(evalExpr(e, next, terms));
      put(out, project.length ? select(next, project) : next, w);
    }
  } else if ('Union' in op) {
    for (const node of op.Union) for (const { row, w } of sub(node).values()) put(out, row, w);
  } else if ('Negate' in op) {
    for (const { row, w } of sub(op.Negate).values()) put(out, row, -w);
  } else if ('Join' in op) {
    const { inputs, equivalences } = op.Join;
    const lk = equivalences.map(eq => eq.find(pair => pair[0] === 0)![1]);
    const rk = equivalences.map(eq => eq.find(pair => pair[0] === 1)![1]);
    const right = groups(sub(inputs[1]), rk);
    for (const l of sub(inputs[0]).values()) {
      for (const r of right.get(keyOf(l.row, lk))?.values() ?? []) put(out, [...l.row, ...r.row], l.w * r.w);
    }
  } else if ('Antijoin' in op) {
    const { l, r, lk, rk } = op.Antijoin;
    const right = new Map<string, number>();
    for (const value of sub(r).values()) {
      const key = keyOf(value.row, rk);
      right.set(key, (right.get(key) ?? 0) + value.w);
    }
    for (const { row, w } of sub(l).values()) if ((right.get(keyOf(row, lk)) ?? 0) <= 0) put(out, row, w);
  } else if ('Reduce' in op) {
    const { input, key, aggs } = op.Reduce;
    const types = typesOf(input);
    for (const [keyText, group] of groups(sub(input), key)) {
      const members = entries(group);
      const count = members.reduce((n, item) => n + item.w, 0);
      if (count <= 0) continue;
      const values = aggs.map(agg => {
        if (agg === 'Count') return count;
        if ('Sum' in agg) return members.reduce((n, item) => n + item.row[agg.Sum] * item.w, 0);
        const col = 'Min' in agg ? agg.Min : agg.Max;
        const live = members.filter(item => item.w > 0).map(item => item.row[col]);
        live.sort((a, b) => types[col] === 'Id' ? termCompare(terms, a, b) : a - b);
        return 'Min' in agg ? live[0] : live[live.length - 1];
      });
      put(out, [...JSON.parse(keyText) as Row, ...values], 1);
    }
  } else if ('Threshold' in op) {
    for (const { row, w } of sub(op.Threshold).values()) if (w > 0) put(out, row, 1);
  } else if ('TopK' in op) {
    const { input, key, order, limit } = op.TopK;
    for (const group of groups(sub(input), key).values()) {
      const live = entries(group).filter(item => item.w > 0);
      live.sort((a, b) => rank(a.row, b.row, order, typesOf(input), terms));
      let left = limit;
      for (const { row, w } of live) {
        if (!left) break;
        const take = Math.min(w, left);
        put(out, row, take);
        left -= take;
      }
    }
  } else if ('Window' in op) {
    const { input, partition, order, func } = op.Window;
    const types = typesOf(input);
    for (const group of groups(sub(input), partition).values()) {
      const rows = entries(group).flatMap(({ row, w }) => Array.from({ length: Math.max(0, w) }, () => row));
      rows.sort((a, b) => rank(a, b, order, types, terms));
      const valueCol = order[0]?.col ?? 0;
      const totalSum = typeof func === 'object' && 'Sum' in func ? rows.reduce((n, row) => n + row[func.Sum], 0) : 0;
      let dense = 0, rankAt = 0, prefixSum = 0;
      rows.forEach((row, i) => {
        if (i === 0 || order.some(o => row[o.col] !== rows[i - 1][o.col])) { dense++; rankAt = i + 1; }
        let value: number;
        if (func === 'RowNumber') value = i + 1;
        else if (func === 'Rank') value = rankAt;
        else if (func === 'DenseRank') value = dense;
        else if (func === 'Count') value = order.length ? i + 1 : rows.length;
        else if ('Lag' in func) value = rows[i - func.Lag]?.[valueCol] ?? 0;
        else if ('Lead' in func) value = rows[i + func.Lead]?.[valueCol] ?? 0;
        else { prefixSum += row[func.Sum]; value = order.length ? prefixSum : totalSum; }
        put(out, [...row, value], 1);
      });
    }
  } else throw new Error('Delay unsupported');
  cache.set(id, out);
  return out;
}

type Frame = { rels: Map<number, Bag>; nodes: Map<number, Bag>; terms: Terms; streams: Map<number, import('rxjs').Observable<NodeStep>> };
type Round = { rels: Map<number, Bag>; terms: Terms; changed: boolean; n: number };

function stratum$(frame: Frame, stratum: Stratum, previous: State) {
  if ('Let' in stratum) {
    const rels = new Map(frame.rels);
    const nodes = new Map(frame.nodes);
    return advanceNode$(stratum.Let.body, previous.rels, rels, previous.nodes, nodes, frame.terms, frame.streams).pipe(
      map(result => {
        rels.set(stratum.Let.id, result.now);
        return { rels, nodes, terms: frame.terms, streams: frame.streams };
      }),
    );
  }
  const rec = stratum.LetRec;
  const firstRels = new Map(frame.rels);
  for (const id of rec.ids) firstRels.set(id, empty());
  const next = (round: Round): Round => {
    if (round.n > 1000) throw new Error('LetRec did not converge');
    const rels = new Map(round.rels);
    const cache = new Map<number, Bag>();
    rec.bodies.forEach((body, i) => {
      const bag = empty();
      for (const { row, w } of evaluate(body, round.rels, round.terms, cache).values()) if (w > 0) put(bag, row, 1);
      rels.set(rec.ids[i], bag);
    });
    return { rels, terms: round.terms, changed: rec.ids.some(id => !same(round.rels.get(id)!, rels.get(id)!)), n: round.n + 1 };
  };
  return defer(() => of(next({ rels: firstRels, terms: frame.terms, changed: true, n: 0 })).pipe(
    expand(round => round.changed ? of(next(round)) : EMPTY),
    reduce((acc, round) => {
      for (const id of rec.ids) {
        const change = diff(acc.last.get(id) ?? empty(), round.rels.get(id) ?? empty());
        const net = acc.delta.get(id) ?? empty();
        for (const { row, w } of change.values()) put(net, row, w);
        acc.delta.set(id, net);
      }
      return { last: round.rels, delta: acc.delta, terms: round.terms };
    }, { last: previous.rels, delta: new Map<number, Bag>(), terms: frame.terms }),
    map(acc => {
      const rels = new Map(acc.last);
      for (const id of rec.ids) {
        const bag = copy(previous.rels.get(id) ?? empty());
        for (const { row, w } of acc.delta.get(id)?.values() ?? []) put(bag, row, w);
        rels.set(id, bag);
      }
      return { rels, nodes: frame.nodes, terms: acc.terms, streams: frame.streams };
    }),
  ));
}

function initialState(): State {
  const rels = new Map<number, Bag>();
  for (const rel of program.rels) if (rel.kind === 'Source') rels.set(rel.id, empty());
  return { rels, nodes: new Map(), outputs: new Map(), terms: initialTerms(), tick: 0, changes: [] };
}

function step$(state: State, frontier: Frontier) {
  const rels = new Map(state.rels);
  for (const { rel, row, w } of frontier.changes) {
    const source = program.rels.find(r => r.id === rel);
    if (source?.kind !== 'Source') throw new Error(`unknown source relation ${rel}`);
    const bag = rels.get(rel) === state.rels.get(rel) ? copy(rels.get(rel) ?? empty()) : rels.get(rel)!;
    rels.set(rel, bag);
    const before = bag.get(JSON.stringify(row))?.w ?? 0;
    if (w > 0 && before > 0) throw new Error(`present insert into relation ${rel}`);
    if (w < 0 && before <= 0) continue;
    put(bag, row, w);
  }
  const terms = structuredClone(state.terms);
  const initial: Frame = { rels, nodes: new Map(), terms, streams: new Map() };
  return from(program.strata).pipe(
    mergeScan((frame: Frame, stratum) => stratum$(frame, stratum, state), initial, 1),
    reduce((_: Frame, frame) => frame, initial),
    map(frame => {
      const outputs = new Map<number, Bag>();
      const changes: Change[] = [];
      for (const rel of program.outputs) {
        const now = frame.rels.get(rel) ?? empty();
        outputs.set(rel, now);
        for (const { row, w } of diff(state.outputs.get(rel) ?? empty(), now).values()) changes.push({ rel, row, w });
      }
      changes.sort((a, b) => a.rel - b.rel || a.row.reduce((order, value, i) => order || value - b.row[i], 0));
      return { rels: frame.rels, nodes: frame.nodes, outputs, terms: frame.terms, tick: state.tick + 1, changes };
    }),
  );
}

/** One settled Z-set batch per accepted frontier. State belongs to the subscription. */
export function run(frontiers$: import('rxjs').Observable<Frontier>): import('rxjs').Observable<DeltaBatch> {
  return defer(() => frontiers$.pipe(
    mergeScan((state: State, frontier) => step$(state, frontier), initialState(), 1),
    map(state => {
      const batch = state.changes as DeltaBatch;
      Object.defineProperty(batch, dictionary, { value: state.terms });
      return batch;
    }),
  ));
}

/** Host input boundary. Each Get reads its relation's changes from these frontier batches. */
export const input = new Subject<Frontier>();
export const outputs$ = run(input.asObservable());
