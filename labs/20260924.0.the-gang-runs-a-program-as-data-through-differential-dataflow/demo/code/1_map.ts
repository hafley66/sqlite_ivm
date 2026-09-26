import relRs from "../../src/1_rel.rs?raw";
import ddRs from "../../src/2_dd.rs?raw";
import ddLike from "./0_dd_like_rxjs.ts?raw";

// Concept token -> plain caption, TS marker, and anchored excerpts of the real engine source.
// Resolution runs at module evaluation and throws when an anchor or marker is missing.

export type RustFile = "2_dd.rs" | "1_rel.rs";
export type Excerpt = { file: RustFile; anchor: string; lines: number | "brace" };
export type Concept = { caption: string; rust: Excerpt[] };

export const rustSources: Record<RustFile, string> = { "2_dd.rs": ddRs, "1_rel.rs": relRs };

export const codeMap: Record<string, Concept> = {
  "code:build": {
    caption: "build: describe the whole dataflow once; nothing runs until changes arrive",
    rust: [
      { file: "2_dd.rs", anchor: "let built = worker.dataflow::<Time, _, _>(|scope|", lines: 1 },
      { file: "2_dd.rs", anchor: "lower(&program, &mut rel)?;", lines: 1 },
      { file: "1_rel.rs", anchor: "pub fn lower<A: Rel>(p: &Program, a: &mut A)", lines: "brace" },
    ],
  },
  "code:input": {
    caption: "input: a table you feed changes into; each change is a row with +1 (add) or −1 (remove)",
    rust: [{ file: "2_dd.rs", anchor: "let (input, c) = scope.new_collection::<Row, W>();", lines: 3 }],
  },
  "code:keyby": {
    caption: "key by: pick the column two tables are matched on",
    rust: [{ file: "2_dd.rs", anchor: "let l = l.map(move |row| (cols(&row, &lk), row));", lines: 2 }],
  },
  "code:join": {
    caption: "join: pair up rows from both sides that share a key",
    rust: [{ file: "2_dd.rs", anchor: "fn join(&mut self, cs: Vec<Self::C>, eq: &[Vec<(u8, ColId)>])", lines: "brace" }],
  },
  "code:map": {
    caption: "map: reshape each row, e.g. keep only the person and the resource",
    rust: [{ file: "2_dd.rs", anchor: "fn mfp(&mut self, c: Self::C, filter: &[Expr]", lines: "brace" }],
  },
  "code:union": {
    caption: "merge / concat: put two streams of changes into one",
    rust: [{ file: "2_dd.rs", anchor: "fn union(&mut self, cs: Vec<Self::C>) -> Self::C {", lines: "brace" }],
  },
  "code:distinct": {
    caption: "distinct / threshold: a row is in the answer once if it has at least one reason",
    rust: [{ file: "2_dd.rs", anchor: "fn threshold(&mut self, c: Self::C) -> Self::C {", lines: "brace" }],
  },
  "code:changes": {
    caption: "changes: watch the output's changes as they happen, tagged with the tick they belong to",
    rust: [{ file: "2_dd.rs", anchor: "for (id, c) in rel.outputs {", lines: "brace" }],
  },
  "code:subscribe": {
    caption: "subscribe: the program's edge, where changes leave the dataflow and the caller reads them",
    rust: [{ file: "2_dd.rs", anchor: "let mut changes: Vec<(RelId, Row, W)> = captured", lines: 8 }],
  },
  "code:update": {
    caption: "update: send one change (+1 add, −1 remove) into an input",
    rust: [{ file: "2_dd.rs", anchor: "for change in accepted {", lines: "brace" }],
  },
  "code:advance": {
    caption: "advance: promise that no more changes come for earlier ticks",
    rust: [{ file: "2_dd.rs", anchor: "epoch += 1;", lines: 5 }],
  },
  "code:step": {
    caption: "step: run the workers until the output has caught up with that promise",
    rust: [
      { file: "2_dd.rs", anchor: "let mut probe = ProbeHandle::new();", lines: 1 },
      { file: "2_dd.rs", anchor: "worker.step_while(|| probe.less_than(&epoch));", lines: 1 },
    ],
  },
};

export type Resolved = { token: string; file: RustFile; start: number; end: number }; // 1-based, inclusive

const resolveExcerpt = (token: string, excerpt: Excerpt): Resolved => {
  const lines = rustSources[excerpt.file].split("\n");
  const hits = lines.flatMap((line, index) => (line.includes(excerpt.anchor) ? [index] : []));
  if (hits.length !== 1) throw new Error(`${token}: anchor ${JSON.stringify(excerpt.anchor)} matches ${hits.length} lines in src/${excerpt.file}; expected exactly 1`);
  const start = hits[0];
  if (excerpt.lines !== "brace") return { token, file: excerpt.file, start: start + 1, end: start + excerpt.lines };
  let depth = 0;
  let opened = false;
  for (let index = start; index < lines.length; index++) {
    for (const char of lines[index]) {
      if (char === "{") (depth++, (opened = true));
      if (char === "}") depth--;
    }
    if (opened && depth <= 0) return { token, file: excerpt.file, start: start + 1, end: index + 1 };
  }
  throw new Error(`${token}: no closing brace after ${JSON.stringify(excerpt.anchor)} in src/${excerpt.file}`);
};

// TS markers: a trailing `// @code:a @code:b +N` tags this line and N following lines; the renderer strips it.
const marker = /\s*\/\/\s*((?:@code:[a-z]+\s*)+)(?:\+(\d+))?\s*$/;

export const parseTs = (source: string) => {
  const lines: string[] = [];
  const tagged = new Map<number, string[]>();
  source.replace(/\n$/, "").split("\n").forEach((line, index) => {
    const found = line.match(marker);
    lines.push(found ? line.slice(0, found.index) : line);
    if (!found) return;
    const found_tokens = found[1].trim().split(/\s+/).map((tag) => tag.slice(1));
    for (let offset = 0; offset <= Number(found[2] ?? 0); offset++) {
      tagged.set(index + 1 + offset, [...(tagged.get(index + 1 + offset) ?? []), ...found_tokens]);
    }
  });
  return { code: lines.join("\n"), tagged };
};

export const ts = parseTs(ddLike);
export const rust = Object.entries(codeMap).flatMap(([token, concept]) => concept.rust.map((excerpt) => resolveExcerpt(token, excerpt)));

for (const token of Object.keys(codeMap)) {
  if (![...ts.tagged.values()].some((tokens) => tokens.includes(token))) throw new Error(`${token}: no // @${token} marker in code/0_dd_like_rxjs.ts`);
}
for (const tokens of ts.tagged.values()) {
  for (const token of tokens) if (!(token in codeMap)) throw new Error(`${token}: marker in code/0_dd_like_rxjs.ts has no entry in code/1_map.ts`);
}

// Excerpts of one file that overlap or touch merge into one block.
export const rustBlocks = (["2_dd.rs", "1_rel.rs"] as RustFile[]).flatMap((file) => {
  const ranges = rust.filter((excerpt) => excerpt.file === file).sort((a, b) => a.start - b.start);
  const blocks: { file: RustFile; start: number; end: number }[] = [];
  for (const range of ranges) {
    const last = blocks[blocks.length - 1];
    if (last && range.start <= last.end + 2) last.end = Math.max(last.end, range.end);
    else blocks.push({ file, start: range.start, end: range.end });
  }
  return blocks;
});

export const rustTokensAt = (file: RustFile, line: number) =>
  rust.filter((excerpt) => excerpt.file === file && excerpt.start <= line && line <= excerpt.end).map((excerpt) => excerpt.token);
