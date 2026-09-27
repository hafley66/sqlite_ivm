import type { Core, EdgeSingular, NodeSingular, Position } from "cytoscape";
import type { Change, Trace } from "./0_trace";
import { emitted, isEntity, rowKey, rowText, tokens } from "./1_labels";
import { hover } from "./2_hover";
import { shownIndex, type Wave } from "./3a_wave";
import { ROW_CAP, type Group, type Packet, type Schedule } from "./3b_schedule";

// Paints the graph for one clock value. Every element it owns (row edges, "+N more" edges, packets,
// badges) is created, updated or removed here from (schedule, clock) alone.

type WaveShape = Pick<Wave, "from" | "to" | "unit" | "depth" | "max_depth">;
type Span = [number, number] | null; // visible part of an edge path, as fractions from source to target

const clamp = (value: number) => Math.max(0, Math.min(1, value));
const progress = (clock: number, start: number, end: number) => (end <= start ? 1 : clamp((clock - start) / (end - start)));

const conceptOf = (names: Trace["names"], change: Change) =>
  [tokens.row(change.row), ...change.row.filter((value) => isEntity(names, value)).map(tokens.entity)].join(" ");

// ---- edge geometry: Cytoscape's bezier as a chain of quadratic segments through the control points ----

const pointAt = (edge: EdgeSingular, t: number): Position => {
  const source = edge.sourceEndpoint();
  const target = edge.targetEndpoint();
  const controls = (edge.controlPoints?.() ?? []) as Position[];
  if (controls.length === 0) return { x: source.x + (target.x - source.x) * t, y: source.y + (target.y - source.y) * t };
  const middle = (a: Position, b: Position) => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });
  const ends = [source, ...controls.slice(1).map((control, index) => middle(controls[index], control)), target];
  const scaled = Math.min(controls.length - 0.000001, t * controls.length);
  const segment = Math.floor(scaled);
  const local = scaled - segment;
  const [a, c, b] = [ends[segment], controls[segment], ends[segment + 1]];
  const u = 1 - local;
  return { x: u * u * a.x + 2 * u * local * c.x + local * local * b.x, y: u * u * a.y + 2 * u * local * c.y + local * local * b.y };
};

const pathLength = (edge: EdgeSingular) => {
  let length = 0;
  let previous = pointAt(edge, 0);
  for (let index = 1; index <= 16; index++) {
    const next = pointAt(edge, index / 16);
    length += Math.hypot(next.x - previous.x, next.y - previous.y);
    previous = next;
  }
  return length;
};

// A partly drawn edge is one dash: [visible length, long gap], shifted so it starts at span[0].
const drawSpan = (edge: EdgeSingular, span: Span) => {
  if (span && span[0] <= 0 && span[1] >= 1) {
    edge.removeStyle("line-style line-dash-pattern line-dash-offset opacity target-arrow-shape events");
    return;
  }
  if (!span || span[1] - span[0] <= 0.001) {
    edge.style({ opacity: 0, events: "no" });
    return;
  }
  const length = pathLength(edge) || 1;
  const dash = (span[1] - span[0]) * length;
  const gap = length * 3;
  edge.style({
    opacity: 1,
    events: "yes",
    "line-style": "dashed",
    "line-dash-pattern": [dash, gap],
    "line-dash-offset": dash + gap - span[0] * length,
    "target-arrow-shape": span[1] >= 1 ? "triangle" : "none",
  });
};

// ---- row edges ----

type RowState = { key: string; change: Change; span: Span };

const rowStates = (group: Group | undefined, fallback: Change[], clock: number, forward: boolean): { rows: RowState[]; more: number } => {
  const full: Span = [0, 1];
  if (!group) return { rows: fallback.slice(0, ROW_CAP).map((change) => ({ key: rowKey(change.row), change, span: full })), more: Math.max(0, fallback.length - ROW_CAP) };
  const before = group.rows_before.slice(0, ROW_CAP);
  const after = group.rows_after.slice(0, ROW_CAP);
  const started = clock >= group.start;
  if (!forward) {
    const shown = started ? after : before;
    return { rows: shown.map((change) => ({ key: rowKey(change.row), change, span: full })), more: Math.max(0, (started ? group.rows_after : group.rows_before).length - ROW_CAP) };
  }
  const packetOf = new Map(group.packets.filter((packet) => packet.row).map((packet) => [rowKey(packet.row!), packet]));
  const afterKeys = new Map(after.map((change) => [rowKey(change.row), change]));
  const beforeKeys = new Map(before.map((change) => [rowKey(change.row), change]));
  const states: RowState[] = [];
  for (const [key, change] of beforeKeys) {
    if (afterKeys.has(key)) continue;
    const gone = progress(clock, group.start, group.start + 0.3 * (group.end - group.start));
    states.push({ key, change, span: !started ? full : gone < 1 ? [gone, 1] : null });
  }
  for (const [key, change] of afterKeys) {
    const packet = packetOf.get(key);
    const was = beforeKeys.get(key);
    if (!started) {
      states.push({ key, change: was ?? change, span: was ? full : null });
    } else if (was && Math.sign(was.w) === Math.sign(change.w)) {
      states.push({ key, change, span: full });
    } else if (!packet) {
      states.push({ key, change, span: null });
    } else if (change.w > 0) {
      states.push({ key, change, span: [0, progress(clock, packet.arrive, packet.drawn)] });
    } else {
      const flown = progress(clock, packet.start, packet.arrive);
      states.push({ key, change, span: clock < packet.arrive ? [flown, 1] : null });
    }
  }
  return { rows: states, more: Math.max(0, (started ? group.rows_after : group.rows_before).length - ROW_CAP) };
};

const litNow = (concept: string) => {
  const lit = new Set(hover.tokens.filter((token) => !token.startsWith("column:")));
  return concept.split(" ").some((token) => lit.has(token));
};

const paintRowEdges = (cy: Core, trace: Trace, wave: WaveShape, plan: Schedule, clock: number, rowEdges: boolean) => {
  const keep = new Set<string>();
  if (rowEdges) {
    const forward = wave.to >= wave.from;
    cy.edges(".base").forEach((base) => {
      const input = base.data("input") as number;
      const group = plan.groups.find((candidate) => candidate.base === base.id());
      const fallback = emitted(trace.steps[shownIndex(wave, { node: input })], input);
      const { rows, more } = rowStates(group, fallback, clock, forward);
      const columns = trace.nodes.find((node) => node.id === input)?.columns ?? [];
      for (const state of rows) {
        const id = `${base.id()}|${state.key}`;
        keep.add(id);
        let edge = cy.getElementById(id) as unknown as EdgeSingular;
        if (edge.empty()) {
          const concept = conceptOf(trace.names, state.change);
          edge = cy.add({ group: "edges", data: { id, source: base.data("source"), target: base.data("target"), base: base.id(), concept, self: concept, rowlabel: "" }, classes: "row" }) as unknown as EdgeSingular;
          edge.toggleClass("lit", litNow(concept));
        }
        edge.data("rowlabel", `${state.change.w > 0 ? "+" : "−"} ${rowText(trace.names, state.change.row, columns)}`);
        edge.toggleClass("row-plus", state.change.w > 0);
        edge.toggleClass("row-minus", state.change.w < 0);
        drawSpan(edge, state.span);
      }
      if (more > 0) {
        const id = `${base.id()}|more`;
        keep.add(id);
        const edge = cy.getElementById(id);
        if (edge.empty()) cy.add({ group: "edges", data: { id, source: base.data("source"), target: base.data("target"), base: base.id(), label: `+${more} more rows`, concept: "" }, classes: "more" });
        else edge.data("label", `+${more} more rows`);
      }
    });
  }
  cy.edges(".row, .more")
    .filter((edge) => !keep.has(edge.id()))
    .remove();
};

// ---- packets and badges ----

const packetPosition = (cy: Core, group: Group, packet: Packet, flown: number, rowEdges: boolean): Position | null => {
  const t = packet.reverse ? 1 - flown : flown;
  if (group.kind === "story") {
    const node = cy.getElementById(`n${group.target}`) as unknown as NodeSingular;
    if (node.empty()) return null;
    const at = node.position();
    const lift = node.height() / 2 + 70;
    return { x: at.x, y: at.y - lift * (1 - t) };
  }
  const rowEdge = rowEdges && packet.row ? cy.getElementById(`${group.base}|${rowKey(packet.row)}`) : cy.collection();
  const edge = (rowEdge.nonempty() ? rowEdge : cy.getElementById(group.base!)) as unknown as EdgeSingular;
  return edge.empty() ? null : pointAt(edge, t);
};

const badge = (cy: Core, id: string, classes: string, label: string, anchor: NodeSingular, dx: number, dy: number, keep: Set<string>) => {
  keep.add(id);
  const box = anchor.boundingBox({ includeLabels: false, includeOverlays: false, includeUnderlays: false });
  const position = { x: box.x2 + dx, y: box.y1 + dy };
  const element = cy.getElementById(id);
  if (element.empty()) cy.add({ group: "nodes", data: { id, label, concept: "" }, classes, position, grabbable: false, selectable: false });
  else element.data("label", label).position(position);
};

const paintPackets = (cy: Core, plan: Schedule, clock: number, rowEdges: boolean, running: boolean) => {
  const keep = new Set<string>();
  const arriving = new Set<number>();
  if (running) {
    for (const group of plan.groups) {
      for (const packet of group.packets) {
        if (clock < packet.start || clock >= packet.merged) continue;
        const flown = progress(clock, packet.start, packet.arrive);
        const position = packetPosition(cy, group, packet, flown, rowEdges);
        if (!position) continue;
        const id = `packet|${packet.id}`;
        keep.add(id);
        const merging = clock >= packet.arrive ? progress(clock, packet.arrive, packet.merged) : 0;
        if (merging > 0) arriving.add(packet.reverse && group.source !== null ? group.source : group.target);
        let element = cy.getElementById(id) as unknown as NodeSingular;
        if (element.empty()) {
          element = cy.add({ group: "nodes", data: { id, label: packet.label, concept: packet.concept, self: packet.concept }, classes: "packet", position, grabbable: false, selectable: false }) as unknown as NodeSingular;
          element.toggleClass("packet-plus", packet.w > 0);
          element.toggleClass("packet-minus", packet.w < 0);
          element.toggleClass("packet-more", packet.row === null);
          element.toggleClass("lit", litNow(packet.concept));
        }
        element.position(position);
        element.style({ opacity: 1 - merging, "font-size": 8 * (1 - 0.6 * merging) });
      }
    }
  }
  cy.nodes(".graph").forEach((node) => void node.toggleClass("arrive", arriving.has(node.data("node"))));

  // Badges: a source relation's delta once its Story drops land; an "absorbed" note while a
  // packet that the target swallows is arriving.
  const badges = new Set<string>();
  if (running) {
    for (const group of plan.groups) {
      const target = cy.getElementById(`n${group.target}`) as unknown as NodeSingular;
      if (target.empty()) continue;
      if (group.kind === "story") {
        const first = Math.min(...group.packets.map((packet) => packet.arrive));
        if (clock < first) continue;
        const added = group.rows_after.filter((change) => change.w > 0).length;
        const removed = group.rows_after.filter((change) => change.w < 0).length;
        badge(cy, `badge|src|${group.target}`, "badge badge-src", [added ? `+${added}` : "", removed ? `−${removed}` : ""].filter(Boolean).join(" "), target, -6, -4, badges);
      } else {
        const absorbed = group.packets.filter((packet) => packet.absorbed && clock >= packet.arrive && clock < packet.arrive + 1);
        if (absorbed.length === 0) continue;
        const lines = absorbed.map((packet) => `${packet.label.slice(2)}: ${packet.absorbed}`);
        badge(cy, `badge|abs|${group.target}`, "badge badge-abs", `absorbed\n${lines.join("\n")}`, target, 70, 0, badges);
      }
    }
  }
  cy.nodes(".packet")
    .filter((node) => !keep.has(node.id()))
    .remove();
  cy.nodes(".badge")
    .filter((node) => !badges.has(node.id()))
    .remove();
};

export const paint = (cy: Core, trace: Trace, wave: WaveShape, plan: Schedule, clock: number, rowEdges: boolean) => {
  if (cy.destroyed()) return;
  cy.batch(() => {
    paintRowEdges(cy, trace, wave, plan, clock, rowEdges);
    paintPackets(cy, plan, clock, rowEdges, wave.from !== wave.to);
  });
};
