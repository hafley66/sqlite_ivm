// Mirror of src/1_rel.rs and src/2_dd.rs, as if differential-dataflow were a TypeScript library.
// Each `#region` has exactly as many lines as the Rust it mirrors; line N sits beside line N.
// Not compiled. `Result`/`?` became `throw`, `match` became `switch`, `Option<T>` became `T | undefined`.
import { AntichainRef, InputSession, ProbeHandle, timely, type Coll, type Nest, type Trace } from "differential-dataflow"
import { BTreeMap, compare, equal, fail, mpsc, ok_or, spawn, tracing, zip } from "./std"
import { ErrorKind, EngineError, Stage, ir, type Agg, type ColId, type Delta, type Engine, type Expr, type Frontier } from "./ir"
import type { LetRec, NodeId, Order, Program, RelId, Row, SourceChange, Time, W } from "./ir"
import { attempt, cols, rank, type Built, type Command, type DdTap, type Hook } from "./dd_types"

// #region rel
export abstract class Rel<C> {
  // (Rust: `type C: Clone;` — the collection type is the class parameter C)
  abstract get(rel: RelId): C // throws EngineError
  abstract mfp(c: C, filter: Expr[], map: Expr[], project: ColId[]): C
  abstract union(cs: C[]): C
  abstract negate(c: C): C
  abstract join(cs: C[], eq: [number, ColId][][]): C // throws EngineError
  abstract antijoin(l: C, r: C, lk: ColId[], rk: ColId[]): C
  abstract reduce(c: C, key: ColId[], aggs: Agg[]): C
  abstract threshold(c: C): C
  topk(_c: C, _key: ColId[], _order: Order[], _limit: number): C {
    throw new EngineError(Stage.Install, undefined, ErrorKind.Unsupported("TopK"))
  }
  /** Engine-owned fixpoint: returns one collection per `rec.ids`, built with `lower_node` on the engine's inner algebra. */
  letrec(_p: Program, _rec: LetRec, _defined: [RelId, C][]): C[] {
    throw new EngineError(Stage.Install, undefined, ErrorKind.Unsupported("LetRec"))
  }
  abstract output(rel: RelId, c: C): void
  /** Called by `lower_node` on every node it builds, before memoizing; traced engines tap `c` here. */
  observe(_id: NodeId, c: C): C {
    return c
  }
}
// #endregion

// #region lower
export function lower<C>(p: Program, a: Rel<C>): void {
  const nodes: (C | undefined)[] = new Array(p.nodes.length).fill(undefined)
  const defined: [RelId, C][] = []
  for (const stratum of p.strata) {
    switch (stratum.kind) {
      case "Let": { const { id, body } = stratum
        const c = lower_node(p, a, nodes, defined, body)
        defined.push([id, c])
      } break
      case "LetRec": { const rec = stratum.rec
        const cs = a.letrec(p, rec, defined)
        defined.push(...zip(rec.ids, cs))
      } break
    }
  }
  for (const out of p.outputs) {
    const c = defined
      // (Rust: .iter())
      .find(([id]) => id === out)
      ?.[1] // (Rust: .clone())
      ?? fail(new EngineError(Stage.Install, out, ErrorKind.UnknownRel(out)))
    a.output(out, c)
  }
  return
}
// #endregion

// #region lower_node
export function lower_node<C>(
  p: Program,
  a: Rel<C>,
  nodes: (C | undefined)[],
  defined: [RelId, C][],
  id: NodeId,
): C {
  if (nodes[id] !== undefined) {
    return nodes[id] // (Rust: c.clone())
  }
  const op = p
    .nodes
    [id]
    ?? fail(new EngineError(Stage.Install, undefined, ErrorKind.UnknownNode(id)))
  const sub = (n: NodeId, a: Rel<C>) => lower_node(p, a, nodes, defined, n)
  let c = ((): C => { switch (op.kind) {
    case "Get": { const found = defined.find(([d]) => d === op.rel)
      if (found !== undefined) return found[1] // (Rust: Some((_, c)) => c.clone())
      return a.get(op.rel) // (Rust: None => a.get(*rel)?)
    }
    case "Mfp": { const { input, filter, map, project } = op
      const c = sub(input, a)
      return a.mfp(c, filter, map, project)
    }
    case "Union": { const inputs = op.inputs
      const cs = inputs.map((n) => sub(n, a))
      return a.union(cs)
    }
    case "Negate": { const input = op.input
      const c = sub(input, a)
      return a.negate(c)
    }
    case "Join": { const { inputs, equivalences } = op
      const cs = inputs.map((n) => sub(n, a))
      return a.join(cs, equivalences)
    }
    case "Antijoin": { const { lk, rk } = op
      const l = sub(op.l, a)
      const r = sub(op.r, a)
      return a.antijoin(l, r, lk, rk)
    }
    case "Reduce": { const { input, key, aggs } = op
      const c = sub(input, a)
      return a.reduce(c, key, aggs)
    }
    case "Threshold": { const input = op.input
      const c = sub(input, a)
      return a.threshold(c)
    }
    case "TopK": { const { input, key, order, limit } = op
      const c = sub(input, a)
      return a.topk(c, key, order, limit)
    }
    case "Window": throw new EngineError(Stage.Install, undefined, ErrorKind.Unsupported("Window"))
    case "Delay": throw new EngineError(Stage.Install, undefined, ErrorKind.Unsupported("Delay"))
  } })()
  c = a.observe(id, c)
  nodes[id] = c // (Rust: Some(c.clone()))
  return c
}
// #endregion

// #region dd_rel
export class DdRel<T extends Nest = Time> extends Rel<Coll<T>> {
  // (Rust: `type C = Coll<'s, T>;` — the class parameter above)

  get(rel: RelId): Coll<T> {
    return this.sources
      .get(rel)
      // (Rust: .cloned())
      ?? fail(new EngineError(Stage.Install, rel, ErrorKind.UnknownRel(rel)))
  }

  mfp(c: Coll<T>, filter: Expr[], map: Expr[], project: ColId[]): Coll<T> {
    // (Rust: to_vec() copies the slices so the closure can own them)
    return c.flat_map((row: Row) => {
      if (!filter.every((e) => ir.eval(e, row) !== 0)) {
        return undefined
      }
      for (const e of map) {
        const v = ir.eval(e, row)
        row.push(v)
      }
      return project.length === 0 ? row : cols(row, project)
    })
  }

  union(cs: Coll<T>[]): Coll<T> {
    const rest = cs.values() // (Rust: let mut cs = cs.into_iter())
    const first = rest.next().value ?? fail(new Error("check rejects an empty Union"))
    return rest.reduce((acc, c) => acc.concat(c), first)
  }

  negate(c: Coll<T>): Coll<T> {
    return c.negate()
  }

  join(cs: Coll<T>[], eq: [number, ColId][][]): Coll<T> {
    if (cs.length !== 2) {
      throw new EngineError(Stage.Install, undefined, ErrorKind.Unsupported("Join arity != 2"))
    }
    const side = (input: number): ColId[] =>
      eq
        .map((klass) => (klass.find(([i]) => i === input) ?? fail(new Error("check: class spans both inputs")))[1])
        // (Rust: .collect())
    // (Rust: end of the `side` closure)
    const [lk, rk] = [side(0), side(1)]
    // (Rust: let mut cs = cs.into_iter())
    let [l, r] = [cs[0], cs[1]]
    l = l.map((row) => [cols(row, lk), row])
    r = r.map((row) => [cols(row, rk), row])
    return l.join(r).map(([, [a, b]]) => {
      a.push(...b)
      return a
    })
  }

  antijoin(l: Coll<T>, r: Coll<T>, lk: ColId[], rk: ColId[]): Coll<T> {
    // (Rust: to_vec() so the closures own lk and rk)
    const keys = r
      .map((row) => cols(row, rk))
      .threshold((_, w: W) => (w > 0 ? 1 : 0))
    return l.map((row) => [cols(row, lk), row]).antijoin(keys).map(([, row]) => row)
  }

  reduce(c: Coll<T>, key: ColId[], aggs: Agg[]): Coll<T> {
    // (Rust: to_vec() so the closure owns key and aggs)
    if (aggs.every((a) => a.kind === "Count" || a.kind === "Sum")) {
      return accumulable(c, key, aggs)
    }
    return c.map((row) => [cols(row, key), row])
      .reduce((_k, input: [Row, W][], output: [Row, W][]) => {
        const count: W = input.map(([, w]) => w).reduce((sum, w) => sum + w, 0)
        if (count <= 0) {
          return
        }
        const live = () => input.filter(([, w]) => w > 0).map(([row]) => row)
        const values = aggs
          // (Rust: .iter())
          .map((agg) => { switch (agg.kind) {
            case "Count": return count
            case "Sum": return input.map(([row, w]) => row[agg.col] * w).reduce((sum, x) => sum + x, 0)
            case "Min": return Math.min(...live().map((row) => row[agg.col]))
            case "Max": return Math.max(...live().map((row) => row[agg.col]))
          } })
          // (Rust: .collect())
        output.push([values, 1])
      })
      .map(([k, values]) => {
        k.push(...values)
        return k
      })
  }

  threshold(c: Coll<T>): Coll<T> {
    return c.threshold((_, w: W) => (w > 0 ? 1 : 0))
  }

  topk(c: Coll<T>, key: ColId[], order: Order[], limit: number): Coll<T> {
    // (Rust: to_vec() so the closure owns key and order)
    return c.map((row) => [cols(row, key), row])
      .reduce((_k, input: [Row, W][], output: [Row, W][]) => {
        const live: [Row, W][] = input.filter(([, w]) => w > 0).map(([r, w]) => [r, w])
        live.sort(([a], [b]) => rank(order, a, b))
        let left: W = limit
        for (const [row, w] of live) {
          if (left === 0) {
            break
          }
          const take = Math.min(w, left)
          output.push([row, take]) // (Rust: row.clone())
          left -= take
        }
      })
      .map(([, row]) => row)
  }

  letrec(p: Program, rec: LetRec, defined: [RelId, Coll<T>][]): Coll<T>[] {
    return this.nest.letrec(this, p, rec, defined)
  }

  output(rel: RelId, c: Coll<T>): void {
    this.outputs.push([rel, c])
  }

  observe(id: NodeId, c: Coll<T>): Coll<T> {
    const sink = this.taps; if (sink === undefined) return c
    // (Rust: Rc::clone(sink) so the closure owns a handle)
    return c.inspect(([row, t, w]) => {
      const [tick, round] = t.split()
      sink.push({ node: id, tick, round, row, w }) // (Rust: borrow_mut(), row.clone())
    })
  }
}
// #endregion

// #region accumulable
function accumulable<T extends Nest>(c: Coll<T>, key: ColId[], aggs: Agg[]): Coll<T> {
  const sums: ColId[] = aggs.flatMap((a) => (a.kind === "Sum" ? [a.col] : []))
  return c.explode((row: Row) => {
    const acc: W[] = [1]
    acc.push(...sums.map((c) => row[c]))
    return [[[cols(row, key), undefined], acc]]
  })
  .reduce((_k, input: [undefined, W[]][], output: [Row, W][]) => {
    const acc = input[0][1]
    if (acc[0] <= 0) {
      return
    }
    let next_sum = 1
    const values = aggs
      // (Rust: .iter())
      .map((a) => { switch (a.kind) {
        case "Count": return acc[0]
        default: {
          next_sum += 1
          return acc[next_sum - 1]
        }
      } })
      // (Rust: .collect())
    output.push([values, 1])
  })
  .map(([k, values]) => {
    k.push(...values)
    return k
  })
}
// #endregion

// #region dd
export class Dd { // (Rust: the fields tx and thread live on `pub struct Dd` above)
  static install_observed(program: Program, hook: Hook): Dd {
    return Dd.start(program, hook, false)
  }

  /** Every IR node's output collection gets an `inspect`; `settle_traced` returns what they saw. */
  static install_traced(program: Program): Dd {
    return Dd.start(program, undefined, true)
  }

  /** `settle` plus the node records of this step, consolidated per `(node, tick, round, row)`.
   *  Empty unless installed with `install_traced`. */
  settle_traced(frontier: Frontier): [Delta, DdTap[]] {
    const [reply, answer] = mpsc.channel<[Delta, DdTap[]]>()
    this.tx.send({ kind: "Settle", frontier, reply }) // throws worker_error(Stage.Settle, e)
    return answer.recv() // throws worker_error(Stage.Settle, e), or the EngineError the worker sent
  }

  static start(program: Program, hook: Hook | undefined, traced: boolean): Dd {
    const [tx, rx] = mpsc.channel<Command>()
    const [ready_tx, ready_rx] = mpsc.channel<void>()
    // (Rust: program.clone() moves a copy into the thread)
    const thread = spawn(() => worker(program, hook, traced, rx, ready_tx))
    const worker_gone = () => new EngineError(Stage.Install, undefined, ErrorKind.Worker("worker exited"))
    ready_rx.recv() // throws worker_gone(), or the install error the worker sent
    return Object.assign(new Dd(), { tx, thread })
  }
}
// #endregion

// #region dd_engine
export const DdEngine: Engine<Dd> = {
  install(program: Program): Dd {
    return Dd.start(program, undefined, false)
  },

  settle(self: Dd, frontier: Frontier): Delta {
    return self.settle_traced(frontier)[0]
  },

  snapshot(self: Dd, rel: RelId): [Row, W][] {
    const [reply, answer] = mpsc.channel<[Row, W][]>()
    self.tx.send({ kind: "Snapshot", rel, reply }) // throws worker_error(Stage.Snapshot, e)
    return answer.recv() // throws worker_error(Stage.Snapshot, e), or the EngineError the worker sent
  },
}
// #endregion

// #region worker
function worker(program: Program, hook: Hook | undefined, traced: boolean, rx: mpsc.Receiver<Command>, ready: mpsc.Sender<void>): void {
  // (Rust: rx behind a Mutex so the timely closure may use it)
  // (Rust: hook behind a Mutex for the same reason)
  timely.execute_directly((worker) => {
    if (hook !== undefined) {
      hook(worker)
    }
    const probe = new ProbeHandle()
    const captured: [RelId, Row, Time, W][] = []
    const taps: DdTap[] | undefined = traced ? [] : undefined
    let built = attempt(() => worker.dataflow<Time>((scope): Built => {
      const inputs = new BTreeMap<RelId, InputSession<Time, Row, W>>()
      const guards = new BTreeMap<RelId, Trace>()
      const sources = new BTreeMap<RelId, Coll>()
      for (const rel of program.rels.filter((r) => r.kind === "Source")) {
        const [input, c] = scope.new_collection<Row, W>()
        const guard = c.arrange_by_self() // (Rust: c.clone())
        guard.stream.probe_with(probe)
        inputs.insert(rel.id, input)
        guards.insert(rel.id, guard.trace)
        sources.insert(rel.id, c)
      }
      const rel = new DdRel({ scope, sources, outputs: [], taps })
      lower(program, rel)
      const outputs = new BTreeMap<RelId, Trace>()
      for (const [id, c] of rel.outputs) {
        const arranged = c.arrange_by_self()
        const sink = captured // (Rust: Rc::clone(&captured))
        arranged
          // (Rust: .clone())
          .as_collection((row: Row) => row)
          .inspect(([row, t, w]) => sink.push([id, row, t, w]))
          .probe_with(probe)
        outputs.insert(id, arranged.trace)
      }
      return { inputs, guards, outputs }
    }))
    switch (built.kind) {
      case "Ok": {
        ready.send({ kind: "Ok" })
        built = built.value; break
      }
      case "Err": { const e = built.error
        ready.send({ kind: "Err", error: e })
        return
      }
    }
    let epoch: Time = 0
    loop: for (;;) {
      const command = rx.recv(); if (command === undefined) break
      switch (command.kind) {
        case "Settle": { const { frontier, reply } = command
          const accepted = ((): SourceChange[] | undefined => { try { return guard(program, built.guards, epoch, frontier) }
            // (Rust: Ok(accepted) => accepted,)
            catch (e) {
              reply.send({ kind: "Err", error: e })
              return undefined
            }
          })(); if (accepted === undefined) continue
          for (const change of accepted) {
            built.inputs.get(change.rel)!.update(change.row, change.w)
          }
          epoch += 1
          for (const input of built.inputs.values()) {
            input.advance_to(epoch)
            input.flush()
          }
          worker.step_while(() => probe.less_than(epoch))
          for (const trace of [...built.guards.values(), ...built.outputs.values()]) {
            trace.set_logical_compaction(new AntichainRef([epoch]))
            trace.set_physical_compaction(new AntichainRef([epoch]))
          }
          const changes: [RelId, Row, W][] = captured
            // (Rust: .borrow_mut())
            .splice(0)
            .map(([rel, row, t, w]) => {
              console.assert(t === epoch - 1)
              return [rel, row, w] as [RelId, Row, W]
            })
            // (Rust: .collect())
          changes.sort(compare)
          const seen = new BTreeMap<[NodeId, number, number | undefined, Row], W>()
          for (const tap of (taps ?? []).splice(0)) {
            seen.insert([tap.node, tap.tick, tap.round, tap.row], (seen.get([tap.node, tap.tick, tap.round, tap.row]) ?? 0) + tap.w)
          }
          const seen_taps = seen // (Rust: `let seen = seen` shadows the map)
            .entries()
            .filter(([, w]) => w !== 0)
            .map(([[node, tick, round, row], w]): DdTap => ({ node, tick, round, row, w }))
            .toArray() // (Rust: .collect())
          reply.send({ kind: "Ok", value: [{ tick: epoch - 1, changes }, seen_taps] })
        } break
        case "Snapshot": { const { rel, reply } = command
          const answer = ok_or(built
            .outputs
            .get(rel),
            read_trace,
            () => new EngineError(Stage.Snapshot, rel, ErrorKind.UnknownRel(rel)))
          reply.send(answer)
        } break
        case "Stop": break loop
      }
    }
  })
}
// #endregion

// #region count
function count(trace: Trace, row: Row): W {
  const [cursor, storage] = trace.cursor()
  cursor.seek_key(storage, row)
  let n: W = 0
  if (equal(cursor.get_key(storage), row)) {
    cursor.map_times(storage, (_, w) => { n += w })
  }
  return n
}
// #endregion

// #region read_trace
function read_trace(trace: Trace): [Row, W][] {
  const [cursor, storage] = trace.cursor()
  const rows: [Row, W][] = []
  for (let row; (row = cursor.get_key(storage)) !== undefined; ) {
    // (Rust: let row = row.clone())
    let n: W = 0
    cursor.map_times(storage, (_, w) => { n += w })
    if (n !== 0) {
      rows.push([row, n])
    }
    cursor.step_key(storage)
  }
  return rows
}
// #endregion

// #region guard
function guard(
  program: Program,
  guards: BTreeMap<RelId, Trace>,
  tick: Time,
  frontier: Frontier,
): SourceChange[] {
  const pending = new BTreeMap<[RelId, Row], W>()
  const accepted: SourceChange[] = []
  for (const change of frontier.changes) {
    const rel = [program
      .rel(change.rel)]
      .find((r) => r?.kind === "Source")
      ?? fail(new EngineError(Stage.Settle, change.rel, ErrorKind.UnknownRel(change.rel)))
    if (change.row.length !== rel.cols.length) {
      const kind = ErrorKind.Arity({ expected: rel.cols.length, actual: change.row.length })
      throw new EngineError(Stage.Settle, rel.id, kind)
    }
    if (change.w !== 1 && change.w !== -1) {
      throw new EngineError(Stage.Settle, rel.id, ErrorKind.Unsupported("weight other than +1/-1"))
    }
    const key: [RelId, Row] = [rel.id, change.row] // (Rust: change.row.clone())
    const before = (pending.get(key) ?? 0) + count(guards.get(rel.id)!, change.row)
    if (change.w > 0 && before > 0) {
      throw new EngineError(Stage.Settle, rel.id, ErrorKind.PresentInsert(change.row))
    }
    if (change.w < 0 && before <= 0) {
      tracing.warn({ tick, relation: rel.name, row: change.row }, "delete of absent row ignored")
      continue
    }
    pending.insert(key, (pending.get(key) ?? 0) + change.w)
    accepted.push(change) // (Rust: change.clone())
  }
  return accepted
}
// #endregion
