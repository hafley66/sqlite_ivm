import relRs from "../../src/1_rel.rs?raw";
import ddRs from "../../src/2_dd.rs?raw";
import mirrorTs from "./0_dd.mirror.ts?raw";

// Regions of the real engine source, each paired with a `// #region <id>` of the TS mirror.
// Resolution runs at module evaluation and throws on a missing anchor or a line-count mismatch.

export type RustFile = "1_rel.rs" | "2_dd.rs";
export type RegionSpec = { id: string; file: RustFile; anchor: string; title: string; caption: string };

export const rustSources: Record<RustFile, string> = { "1_rel.rs": relRs, "2_dd.rs": ddRs };

export const regionSpecs: RegionSpec[] = [
  { id: "rel", file: "1_rel.rs", anchor: "pub trait Rel {", title: "trait Rel", caption: "The operator algebra. Every engine implements these methods; lower() only talks to this interface." },
  { id: "lower", file: "1_rel.rs", anchor: "pub fn lower<A: Rel>(p: &Program, a: &mut A)", title: "fn lower", caption: "Walk the program's strata in order, build each defined relation once, then hand the outputs to the engine." },
  { id: "lower_node", file: "1_rel.rs", anchor: "pub fn lower_node<A: Rel>(", title: "fn lower_node", caption: "Build one IR node: build its inputs first (memoized), then call the engine's operator for the node's kind." },
  { id: "dd_rel", file: "2_dd.rs", anchor: "impl<'s, T: Nest> Rel for DdRel<'s, T> {", title: "impl Rel for DdRel", caption: "The differential-dataflow version of each operator, written as collection transformations." },
  { id: "accumulable", file: "2_dd.rs", anchor: "fn accumulable<'s, T: Nest>(", title: "fn accumulable", caption: "Count and sum ride inside the weight, so a group stays one record and each change costs the same." },
  { id: "dd", file: "2_dd.rs", anchor: "impl Dd {", title: "impl Dd", caption: "Start the worker thread and talk to it over a channel." },
  { id: "dd_engine", file: "2_dd.rs", anchor: "impl Engine for Dd {", title: "impl Engine for Dd", caption: "Install, settle one step, read a snapshot: each is a message to the worker." },
  { id: "worker", file: "2_dd.rs", anchor: "fn worker(program: Program", title: "fn worker", caption: "Build the dataflow once, then answer Settle / Snapshot / Stop commands until told to stop." },
  { id: "count", file: "2_dd.rs", anchor: "fn count(trace: &mut Trace, row: &Row) -> W {", title: "fn count", caption: "How many copies of a row a source table holds right now." },
  { id: "read_trace", file: "2_dd.rs", anchor: "fn read_trace(trace: &mut Trace)", title: "fn read_trace", caption: "List every row of an arranged output with its current count." },
  { id: "guard", file: "2_dd.rs", anchor: "fn guard(", title: "fn guard", caption: "Check one step's source changes before they enter the dataflow: tables are sets." },
];

// `anchor` must match exactly one Rust line of the region, and that line must open a block.
export const blockCaptions: { region: string; anchor: string; caption: string }[] = [
  { region: "lower", anchor: "for stratum in &p.strata {", caption: "Each stratum defines relations; earlier ones are visible to later ones." },
  { region: "lower_node", anchor: "let c = match op {", caption: "Dispatch on the node's kind; each arm builds its inputs first (recursive, memoized)." },
  { region: "lower_node", anchor: "Op::Get(rel) =>", caption: "Get arm: reuse a relation defined earlier in the program, else read the source table." },
  { region: "lower_node", anchor: "Op::Mfp {", caption: "Mfp arm: build the input, then filter, compute and project each row." },
  { region: "lower_node", anchor: "Op::Union(inputs) =>", caption: "Union arm: build every input and put their changes into one stream." },
  { region: "lower_node", anchor: "Op::Join { inputs, equivalences } =>", caption: "Join arm: build both inputs, then ask the engine to pair rows whose key columns are equal." },
  { region: "lower_node", anchor: "Op::Antijoin {", caption: "Antijoin arm: keep left rows whose key has no match on the right." },
  { region: "lower_node", anchor: "Op::Reduce {", caption: "Reduce arm: group by key and aggregate." },
  { region: "lower_node", anchor: "Op::Threshold(input) =>", caption: "Threshold arm: a row is present once if it has any positive weight." },
  { region: "dd_rel", anchor: "fn join(", caption: "join: key both sides by the equivalence columns, join, then glue the two rows together." },
  { region: "dd_rel", anchor: "fn reduce(", caption: "reduce: count and sum take the fast path; min and max regroup the live rows of each key." },
  { region: "dd_rel", anchor: "fn threshold(", caption: "threshold: weight above zero becomes 1; otherwise the row disappears." },
  { region: "dd_rel", anchor: "fn observe(", caption: "observe: in traced mode every node's changes are copied into taps (this page's data)." },
  { region: "worker", anchor: "let built = worker.dataflow::<Time, _, _>(", caption: "Build the dataflow once: an input per source table, lower the program, capture every output's changes." },
  { region: "worker", anchor: "for rel in program.rels.iter().filter(", caption: "One input per source table, arranged so guard() can count what is present." },
  { region: "worker", anchor: "for (id, c) in rel.outputs {", caption: "Each output is arranged (for snapshots) and inspected (to capture this step's changes)." },
  { region: "worker", anchor: "let mut built = match built {", caption: "Tell the caller whether install worked." },
  { region: "worker", anchor: "loop {", caption: "The command loop: one Settle per step until Stop." },
  { region: "worker", anchor: "Command::Settle(frontier, reply) =>", caption: "Settle arm: check the changes, feed them in, advance the epoch, run until the probe passes it, collect what the outputs emitted." },
  { region: "worker", anchor: "let accepted = match guard(", caption: "Run the set checks; a rejected step is answered with the error and skipped." },
  { region: "worker", anchor: "for input in built.inputs.values_mut() {", caption: "Advance: promise that no more changes come for earlier ticks." },
  { region: "worker", anchor: "Command::Snapshot(rel, reply) =>", caption: "Snapshot arm: read the current contents of one output's arrangement." },
  { region: "guard", anchor: "for change in &frontier.changes {", caption: "Each change: known source table? right number of columns? weight ±1? then the set rules." },
];

export type Block = { start: number; end: number }; // 0-based indexes into the region's lines
export type Region = RegionSpec & {
  rust_start: number; // 1-based line in src/<file>
  rust_lines: string[];
  ts_start: number; // 1-based line in code/0_dd.mirror.ts
  ts_lines: string[];
  block_of: Block[]; // per line: the block that lights when the line is hovered
  within: Block[][]; // per line: every block containing it, so hovering an outer block lights nested lines
  captions: Map<string, string>; // blockKey -> caption
};

export const blockKey = (region: string, block: Block) => `code:blk:${region}:${block.start}-${block.end}`;
export const rowKey = (region: string, index: number) => `code:row:${region}:${index}`;

const withoutLiterals = (line: string) => line.replace(/"(?:\\.|[^"\\])*"/g, '""').replace(/'(?:\\.|[^'\\])'/g, "''");

const rustRange = (spec: RegionSpec) => {
  const lines = rustSources[spec.file].split("\n");
  const hits = lines.flatMap((line, index) => (line.includes(spec.anchor) ? [index] : []));
  if (hits.length !== 1) throw new Error(`region ${spec.id}: anchor ${JSON.stringify(spec.anchor)} matches ${hits.length} lines in src/${spec.file}; expected exactly 1`);
  let depth = 0;
  let opened = false;
  for (let index = hits[0]; index < lines.length; index++) {
    for (const char of withoutLiterals(lines[index])) {
      if (char === "{") (depth++, (opened = true));
      if (char === "}") depth--;
    }
    if (opened && depth <= 0) return { start: hits[0] + 1, lines: lines.slice(hits[0], index + 1) };
  }
  throw new Error(`region ${spec.id}: no closing brace after ${JSON.stringify(spec.anchor)} in src/${spec.file}`);
};

const mirrorRegions = () => {
  const found = new Map<string, { start: number; lines: string[] }>();
  let current: { id: string; start: number; lines: string[] } | null = null;
  mirrorTs.split("\n").forEach((line, index) => {
    const open = line.match(/^\/\/ #region (\S+)/);
    if (open) current = { id: open[1], start: index + 2, lines: [] };
    else if (line.startsWith("// #endregion") && current) (found.set(current.id, current), (current = null));
    else if (current) current.lines.push(line);
  });
  return found;
};

// A line that opens a multi-line block lights the outermost block it opens; any other line lights
// the innermost block around it; lines outside every block light the whole region.
const pairsOf = (lines: string[]): Block[] => {
  const pairs: Block[] = [];
  const stack: number[] = [];
  lines.forEach((line, index) => {
    for (const char of withoutLiterals(line)) {
      if (char === "{") stack.push(index);
      if (char === "}") {
        const start = stack.pop();
        if (start !== undefined && start < index) pairs.push({ start, end: index });
      }
    }
  });
  return pairs;
};

const blocksOf = (lines: string[], pairs: Block[]): Block[] => {
  const whole = { start: 0, end: lines.length - 1 };
  return lines.map((_, index) => {
    const opened = pairs.filter((pair) => pair.start === index).sort((a, b) => b.end - a.end)[0];
    if (opened) return opened;
    const around = pairs.filter((pair) => pair.start < index && index <= pair.end).sort((a, b) => a.end - a.start - (b.end - b.start))[0];
    return around ?? whole;
  });
};

const mirrors = mirrorRegions();

export const regions: Region[] = regionSpecs.map((spec) => {
  const rust = rustRange(spec);
  const mirror = mirrors.get(spec.id);
  if (!mirror) throw new Error(`region ${spec.id}: no "// #region ${spec.id}" in code/0_dd.mirror.ts`);
  if (mirror.lines.length !== rust.lines.length) {
    throw new Error(`region ${spec.id}: src/${spec.file}:${rust.start} has ${rust.lines.length} lines, mirror has ${mirror.lines.length}; update code/0_dd.mirror.ts`);
  }
  const pairs = pairsOf(rust.lines);
  const block_of = blocksOf(rust.lines, pairs);
  const whole = { start: 0, end: rust.lines.length - 1 };
  const within = rust.lines.map((_, index) => [whole, ...pairs.filter((pair) => pair.start <= index && index <= pair.end)]);
  const captions = new Map<string, string>();
  for (const entry of blockCaptions.filter((candidate) => candidate.region === spec.id)) {
    const hits = rust.lines.flatMap((line, index) => (line.includes(entry.anchor) ? [index] : []));
    if (hits.length !== 1) throw new Error(`caption ${spec.id}: anchor ${JSON.stringify(entry.anchor)} matches ${hits.length} lines; expected exactly 1`);
    const block = block_of[hits[0]];
    if (block.start !== hits[0]) throw new Error(`caption ${spec.id}: ${JSON.stringify(entry.anchor)} does not open a block`);
    captions.set(blockKey(spec.id, block), entry.caption);
  }
  return { ...spec, rust_start: rust.start, rust_lines: rust.lines, ts_start: mirror.start, ts_lines: mirror.lines, block_of, within, captions };
});

for (const id of mirrors.keys()) {
  if (!regionSpecs.some((spec) => spec.id === id)) throw new Error(`mirror region ${id} has no entry in code/1_map.ts`);
}
