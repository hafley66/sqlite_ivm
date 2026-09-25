import { createContext, useContext } from "react";
import type { Change, Row, Trace } from "./0_trace";
import { consolidate, emitted, isEntity, reasons, rowKey, rowText, tokens, weightOf } from "./1_labels";
import { stageUnit, type Wave } from "./3a_wave";

// Pure function of (trace, from, to, depth); the graph repaints from it at every clock value.
// A stage unit splits into sequential groups (one per edge or Story relation); packets stagger.

// Rows drawn per edge; the rest travel as one "+N" packet and one "+N more rows" edge.
export const ROW_CAP = 8;

export type Packet = {
  id: string;
  row: Row | null;         // null for the "+N" packet
  w: number;
  label: string;
  concept: string;
  start: number;           // leaves the source
  arrive: number;          // reaches the target
  merged: number;          // gone into the target
  drawn: number;           // its row edge is fully drawn
  reverse: boolean;        // backward wave: flies target → source
  absorbed: string | null; // the target takes it in and emits nothing for it ("2 reasons → 1 reason")
};

export type Group = {
  id: string;
  unit: number;
  start: number;
  end: number;
  kind: "story" | "edge";
  label: string;
  base: string | null;      // base edge id; null for a Story drop
  source: number | null;    // IR node id; null for a Story drop
  target: number;           // IR node id
  columns: string[];
  rows_before: Change[];    // rows on this edge before the group (from step)
  rows_after: Change[];     // rows on this edge after the group (to step)
  packets: Packet[];
};

export type Schedule = { groups: Group[]; stops: number[] };

export const baseEdgeId = (input: number, node: number, index: number) => `e${input}-${node}-${index}`;

const flip = (changes: Change[]) => changes.map(({ row, w }) => ({ row, w: -w }));

const conceptOf = (names: Trace["names"], changes: Change[]) =>
  [...new Set(changes.flatMap((change) => [tokens.row(change.row), ...change.row.filter((value) => isEntity(names, value)).map(tokens.entity)]))].join(" ");

const shortText = (text: string) => (text.length > 22 ? `${text.slice(0, 21)}…` : text);

type WaveShape = Pick<Wave, "from" | "to" | "unit" | "depth" | "max_depth">;

export const schedule = (trace: Trace, wave: WaveShape): Schedule => {
  if (wave.from === wave.to) return { groups: [], stops: [] };
  const forward = wave.to > wave.from;
  const to = trace.steps[wave.to];
  const from = trace.steps[wave.from];
  // Forward: the new step's changes travel. Backward: the undone step's changes travel back, sign flipped.
  const moving = forward ? to : from;
  const drafts: Omit<Group, "start" | "end" | "packets">[] = [];

  // Story: each source relation receiving frontier rows gets a drop group.
  for (const node of trace.nodes) {
    if (node.op !== "Get" || node.relation === null) continue;
    const relation = trace.relations.find((candidate) => candidate.id === node.relation);
    const rows = consolidate((moving?.frontier ?? []).filter((change) => change.relation === node.relation).map(({ row, w }) => ({ row, w })));
    if (!relation || rows.length === 0) continue;
    drafts.push({
      id: `story-${node.id}`,
      unit: stageUnit(wave, "story"),
      kind: "story",
      label: `into ${relation.name}`,
      base: null,
      source: null,
      target: node.id,
      columns: relation.columns,
      rows_before: [],
      rows_after: forward ? rows : flip(rows),
    });
  }

  // Edges: an edge carries its source's output and runs at the source's stage.
  for (const node of trace.nodes) {
    node.inputs.forEach((input, index) => {
      const before = emitted(from, input);
      const after = emitted(to, input);
      if (before.length === 0 && after.length === 0) return;
      const source = trace.nodes.find((candidate) => candidate.id === input);
      drafts.push({
        id: baseEdgeId(input, node.id, index),
        unit: stageUnit(wave, { node: input }),
        kind: "edge",
        label: `${source?.caption || source?.op || input} → ${node.caption || node.op}`,
        base: baseEdgeId(input, node.id, index),
        source: input,
        target: node.id,
        columns: source?.columns ?? [],
        rows_before: before,
        rows_after: after,
      });
    });
  }

  const groups: Group[] = [];
  const units = [...new Set(drafts.map((draft) => draft.unit))].sort((a, b) => a - b);
  for (const unit of units) {
    const inUnit = drafts.filter((draft) => draft.unit === unit);
    const length = 1 / inUnit.length;
    inUnit.forEach((draft, index) => {
      const start = unit + index * length;
      const travelling =
        draft.kind === "story" ? draft.rows_after : forward ? emitted(to, draft.source!) : flip(emitted(from, draft.source!));
      groups.push({ ...draft, start, end: start + length, packets: packets(trace, draft, travelling, start, length, forward, wave) });
    });
  }
  const stops = [...new Set([...groups.map((group) => group.start), ...Array.from({ length: wave.max_depth + 4 }, (_, unit) => unit)])].sort((a, b) => a - b);
  return { groups, stops };
};

const packets = (
  trace: Trace,
  draft: Omit<Group, "start" | "end" | "packets">,
  travelling: Change[],
  start: number,
  length: number,
  forward: boolean,
  wave: WaveShape,
): Packet[] => {
  const shown = travelling.slice(0, ROW_CAP);
  const hidden = travelling.slice(ROW_CAP);
  const count = shown.length + (hidden.length ? 1 : 0);
  const stagger = count > 1 ? Math.min(0.15, (0.35 * length) / (count - 1)) : 0;
  const flight = 0.55 * length;
  const merge = 0.1 * length;
  const draw = 0.25 * length;
  const target = trace.nodes.find((node) => node.id === draft.target);
  const targetEmits = emitted(trace.steps[wave.to], draft.target);
  const absorbedText = (change: Change) => {
    if (!forward || draft.kind !== "edge" || !target) return null;
    if (target.op === "Threshold") {
      if (weightOf(targetEmits, change.row) !== 0) return null;
      const was = trace.steps.slice(0, wave.to).reduce((sum, step) => sum + weightOf(emitted(step, draft.source!), change.row), 0);
      return `${was} → ${reasons(was + change.w)}`;
    }
    if ((target.op === "Reduce" || target.op === "TopK") && targetEmits.length === 0) return "absorbed";
    return null;
  };
  const list: Packet[] = shown.map((change, index) => {
    const at = start + index * stagger;
    return {
      id: `${draft.id}|${rowKey(change.row)}`,
      row: change.row,
      w: change.w,
      label: `${change.w > 0 ? "+" : "−"} ${shortText(rowText(trace.names, change.row, draft.columns))}`,
      concept: conceptOf(trace.names, [change]),
      start: at,
      arrive: at + flight,
      merged: at + flight + merge,
      drawn: at + flight + draw,
      reverse: !forward,
      absorbed: absorbedText(change),
    };
  });
  if (hidden.length) {
    const at = start + shown.length * stagger;
    list.push({
      id: `${draft.id}|more`,
      row: null,
      w: 0,
      label: `+${hidden.length} more`,
      concept: conceptOf(trace.names, hidden),
      start: at,
      arrive: at + flight,
      merged: at + flight + merge,
      drawn: at + flight + draw,
      reverse: !forward,
      absorbed: null,
    });
  }
  return list;
};

export const PlanContext = createContext<Schedule>({ groups: [], stops: [] });
export const usePlan = () => useContext(PlanContext);
