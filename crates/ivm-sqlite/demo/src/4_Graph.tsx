import cytoscape, { type Core, type StylesheetJson } from "cytoscape";
import dagre from "cytoscape-dagre";
import { useEffect, useRef } from "react";
import { animate } from "motion/react";
import type { Change, Trace } from "./0_trace";
import { emitted, isEntity, nodeConcept, nodeTitle, opWord, tokens } from "./1_labels";
import { hover, useHover } from "./2_hover";
import { isActive, pulseOf, shownIndex, unitSeconds, useWave } from "./3a_wave";
import { baseEdgeId, usePlan } from "./3b_schedule";
import { paint } from "./3c_paint";

cytoscape.use(dagre);

const loopParent = "loop";


const changeTokens = (names: Trace["names"], changes: Change[]) =>
  changes.flatMap((change) => [tokens.row(change.row), ...change.row.filter((value) => isEntity(names, value)).map(tokens.entity)]);


type Counts = { added: number; removed: number };

const counts = (changes: Change[]): Counts => ({
  added: changes.filter((change) => change.w > 0).length,
  removed: changes.filter((change) => change.w < 0).length,
});

const edgeLabel = ({ added, removed }: Counts) => [added ? `+${added}` : "", removed ? `−${removed}` : ""].filter(Boolean).join(" ");

// Cytoscape's own style transitions carry the glow and colour changes; `lit` is excluded so hover stays instant.
const stylesheet = (reduced: boolean): StylesheetJson => [
  {
    selector: "node",
    style: {
      shape: "round-rectangle",
      "background-color": "#e0f2fe",
      "border-color": "#0369a1",
      "border-width": 1,
      label: "data(label)",
      "text-wrap": "wrap",
      "text-max-width": "150px",
      "text-valign": "center",
      "font-size": 11,
      color: "#0c4a6e",
      width: "label",
      height: "label",
      padding: "10px",
      "underlay-color": "#f59e0b",
      "underlay-opacity": 0,
      "underlay-padding": 7,
      "underlay-shape": "round-rectangle",
      "transition-property": "border-color, border-width, underlay-color, underlay-opacity, underlay-padding",
      "transition-duration": reduced ? 0 : 280,
    },
  },
  {
    selector: `node[id = "${loopParent}"]`,
    style: {
      "background-color": "#f5f3ff",
      "background-opacity": 0.6,
      "border-color": "#7c3aed",
      "border-style": "dashed",
      label: "data(label)",
      "text-valign": "top",
      "text-halign": "center",
      color: "#6d28d9",
      "font-size": 11,
    },
  },
  { selector: "node.changed", style: { "underlay-opacity": 0.45, "border-color": "#d97706", "border-width": 2 } },
  { selector: "node.active", style: { "underlay-color": "#0ea5e9", "underlay-opacity": 0.6, "underlay-padding": 13, "border-color": "#0284c7", "border-width": 3 } },
  { selector: "node.pulse-plus", style: { "underlay-color": "#10b981", "underlay-opacity": 0.6, "underlay-padding": 11 } },
  { selector: "node.pulse-minus", style: { "underlay-color": "#f43f5e", "underlay-opacity": 0.6, "underlay-padding": 11 } },
  { selector: "node.pulse-node", style: { "underlay-color": "#0ea5e9", "underlay-opacity": 0.6, "underlay-padding": 13 } },
  { selector: "node.selected-node", style: { "border-color": "#1d4ed8", "border-width": 3 } },
  { selector: "node.lit", style: { "background-color": "#fde047" } },
  { selector: "node.arrive", style: { "underlay-color": "#8b5cf6", "underlay-opacity": 0.55, "underlay-padding": 12 } },
  {
    selector: "node.packet",
    style: {
      "background-color": "#64748b",
      "border-width": 0,
      color: "#ffffff",
      "font-size": 8,
      "font-weight": "bold",
      padding: "3px",
      "text-max-width": "160px",
      "z-index": 100,
      "z-compound-depth": "top",
      "transition-duration": 0,
      "underlay-opacity": 0,
    },
  },
  { selector: "node.packet-plus", style: { "background-color": "#059669" } },
  { selector: "node.packet-minus", style: { "background-color": "#e11d48" } },
  { selector: "node.packet.lit", style: { "background-color": "#ca8a04" } },
  {
    selector: "node.badge",
    style: {
      "background-color": "#ffffff",
      "border-width": 1,
      "border-color": "#0f766e",
      color: "#0f766e",
      "font-size": 9,
      "font-weight": "bold",
      padding: "3px",
      "text-max-width": "200px",
      "z-index": 90,
      "z-compound-depth": "top",
      "transition-duration": 0,
      "underlay-opacity": 0,
    },
  },
  { selector: "node.badge-abs", style: { "border-style": "dashed", "border-color": "#64748b", color: "#334155", "background-color": "#f8fafc", "font-weight": "normal" } },
  {
    selector: "edge",
    style: {
      width: 1.5,
      "line-color": "#94a3b8",
      "target-arrow-color": "#94a3b8",
      "target-arrow-shape": "triangle",
      "curve-style": "bezier",
      label: "data(label)",
      "font-size": 11,
      "font-weight": "bold",
      color: "#b45309",
      "text-background-color": "#ffffff",
      "text-background-opacity": 1,
      "text-background-padding": "2px",
      "transition-property": "line-color, target-arrow-color, width",
      "transition-duration": reduced ? 0 : 280,
    },
  },
  { selector: "edge.flowing", style: { width: 3, "line-color": "#f59e0b", "target-arrow-color": "#f59e0b" } },
  // "one line per row": row edges fan out beside the base edge; 3c_paint draws them progressively.
  {
    selector: "edge.row",
    style: {
      width: 2.5,
      label: "",
      "curve-style": "bezier",
      "control-point-step-size": 14,
      "arrow-scale": 0.7,
      "font-size": 9,
      "font-weight": "normal",
      "text-rotation": "autorotate",
      "text-background-opacity": 0.9,
    },
  },
  { selector: "edge.row.row-plus", style: { "line-color": "#10b981", "target-arrow-color": "#10b981", color: "#047857" } },
  { selector: "edge.row.row-minus", style: { "line-color": "#f43f5e", "target-arrow-color": "#f43f5e", color: "#be123c" } },
  // A row sentence along a short edge covers the line, so row edges are labelled on hover.
  { selector: "edge.row.lit", style: { label: "data(rowlabel)" } },
  {
    selector: "edge.more",
    style: { width: 1.5, "line-style": "dotted", "line-color": "#64748b", "target-arrow-color": "#64748b", "curve-style": "bezier", "control-point-step-size": 14, label: "data(label)", "font-size": 9, color: "#334155" },
  },
  { selector: "edge.pulse-plus", style: { width: 5, "line-color": "#10b981", "target-arrow-color": "#10b981" } },
  { selector: "edge.pulse-minus", style: { width: 5, "line-color": "#f43f5e", "target-arrow-color": "#f43f5e" } },
  { selector: "edge.lit", style: { width: 5, "line-color": "#eab308", "target-arrow-color": "#eab308", "text-background-color": "#fde047" } },
  // Last: with row edges drawn, the base edge stays thin and grey whatever else applies.
  { selector: "edge.base.thin", style: { width: 1, "line-style": "solid", "line-color": "#cbd5e1", "target-arrow-color": "#cbd5e1", label: "" } },
];

export const Graph = ({ trace, selected, onSelect, reduced, rowEdges }: { trace: Trace; selected: number | null; onSelect: (id: number) => void; reduced: boolean; rowEdges: boolean }) => {
  const wave = useWave();
  const plan = usePlan();
  const container = useRef<HTMLDivElement>(null);
  const cyRef = useRef<Core | null>(null);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const hovered = useHover();
  // Per base edge: the step whose label it shows, and the running label tween.
  const edgeShown = useRef(new Map<string, number>());
  const labelTweens = useRef(new Map<string, { stop: () => void }>());
  const scene = useRef({ wave, plan, rowEdges });
  scene.current = { wave, plan, rowEdges };
  const repaint = () => {
    const cy = cyRef.current;
    if (cy) paint(cy, trace, scene.current.wave, scene.current.plan, scene.current.wave.clock.get(), scene.current.rowEdges);
  };

  // Build once per trace; layout is stable across steps.
  useEffect(() => {
    const anyLoop = trace.nodes.some((node) => node.in_loop);
    const cy = cytoscape({
      container: container.current,
      style: stylesheet(reduced),
      wheelSensitivity: 0.3,
      maxZoom: 1.25,
      elements: [
        ...(anyLoop ? [{ data: { id: loopParent, label: "repeats until nothing changes", self: "", concept: "" }, classes: "graph" }] : []),
        ...trace.nodes.map((node) => ({
          data: {
            id: `n${node.id}`,
            node: node.id,
            label: (node.caption || node.relation !== null) ? `${nodeTitle(trace, node)}\n(${opWord(node.op)})` : nodeTitle(trace, node),
            parent: node.in_loop ? loopParent : undefined,
            self: nodeConcept(trace, node),
            concept: nodeConcept(trace, node),
          },
          classes: "graph",
        })),
        ...trace.nodes.flatMap((node) =>
          node.inputs.map((input, index) => ({
            data: { id: baseEdgeId(input, node.id, index), source: `n${input}`, target: `n${node.id}`, input, node: node.id, label: "", self: "", concept: "" },
            classes: "base",
          })),
        ),
      ],
      layout: { name: "dagre", rankDir: "TB", nodeSep: 30, rankSep: 55, padding: 16 } as cytoscape.LayoutOptions,
    });
    cy.on("mouseover", "node,edge", (event) => hover.set(String(event.target.data("self") || event.target.data("concept")).split(" ").filter(Boolean)));
    cy.on("mouseout", "node,edge", () => hover.set([]));
    cy.on("tap", "node", (event) => {
      const id = event.target.data("node");
      if (typeof id === "number") onSelectRef.current(id);
    });
    cyRef.current = cy;
    edgeShown.current.clear();
    const resized = new ResizeObserver(() => {
      cy.resize();
      cy.fit(cy.nodes(".graph"), 16);
    });
    resized.observe(container.current!);
    const unsubscribe = scene.current.wave.clock.on("change", repaint);
    repaint();
    return () => {
      unsubscribe();
      resized.disconnect();
      labelTweens.current.forEach((tween) => tween.stop());
      labelTweens.current.clear();
      cy.destroy();
      cyRef.current = null;
    };
  }, [trace, reduced]);

  // A new wave: finish every running label tween.
  useEffect(() => {
    return () => {
      labelTweens.current.forEach((tween) => tween.stop());
      labelTweens.current.clear();
    };
  }, [trace, wave.from, wave.to]);

  // Each wave unit: nodes and edges the wave has reached switch to the new step; the active depth pulses.
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    cy.batch(() => {
      for (const node of trace.nodes) {
        const stage = { node: node.id };
        const changes = emitted(trace.steps[shownIndex(wave, stage)], node.id);
        const element = cy.getElementById(`n${node.id}`);
        element.toggleClass("active", isActive(wave, stage) && changes.length > 0);
        element.toggleClass("changed", changes.length > 0);
        element.data("concept", [nodeConcept(trace, node), ...changeTokens(trace.names, changes)].join(" "));
      }
    });
    cy.edges(".base").forEach((edge) => {
      // An edge carries its source node's output, so it changes and pulses at the source's stage.
      const input = edge.data("input") as number;
      const shown = shownIndex(wave, { node: input });
      const changes = emitted(trace.steps[shown], input);
      edge.data("concept", changeTokens(trace.names, changes).join(" "));
      edge.toggleClass("flowing", changes.length > 0);
      edge.toggleClass("thin", rowEdges);

      const previous = edgeShown.current.get(edge.id());
      if (previous !== shown) {
        edgeShown.current.set(edge.id(), shown);
        const target = counts(changes);
        labelTweens.current.get(edge.id())?.stop();
        if (previous === undefined || reduced) {
          edge.data("label", edgeLabel(target));
        } else {
          const start = counts(emitted(trace.steps[previous], input));
          const tween = animate(0, 1, {
            duration: unitSeconds(wave) * 1.5,
            onUpdate: (progress) =>
              edge.data(
                "label",
                edgeLabel({
                  added: Math.round(start.added + (target.added - start.added) * progress),
                  removed: Math.round(start.removed + (target.removed - start.removed) * progress),
                }),
              ),
            onComplete: () => edge.data("label", edgeLabel(target)),
          });
          labelTweens.current.set(edge.id(), { stop: () => (tween.stop(), edge.data("label", edgeLabel(target))) });
        }
      }
    });
    repaint();

    // Pulse: elements carrying a token the wave pulses this unit; removing the class next unit fades it out.
    // The graph pulses on row and node tokens; entity tokens would flash every edge that ever carried that name.
    const pulse = reduced ? { plus: [], minus: [], node: [] as string[] } : pulseOf(trace, wave);
    const plus = new Set(pulse.plus.filter((token) => token.startsWith("row:")));
    const minus = new Set(pulse.minus.filter((token) => token.startsWith("row:")));
    const nodeTokens = new Set(pulse.node);
    cy.batch(() =>
      cy.elements().forEach((element) => {
        const concept = String(element.data("concept") ?? "").split(" ");
        const isNode = concept.some((token) => nodeTokens.has(token));
        element.toggleClass("pulse-node", isNode);
        element.toggleClass("pulse-minus", !isNode && concept.some((token) => minus.has(token)));
        element.toggleClass("pulse-plus", !isNode && concept.some((token) => plus.has(token)));
      }),
    );
  }, [trace, wave.from, wave.to, wave.unit, wave.speed, reduced, rowEdges, plan]);

  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    cy.nodes().removeClass("selected-node");
    if (selected !== null) cy.getElementById(`n${selected}`).addClass("selected-node");
  }, [trace, selected, reduced]);

  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    const lit = new Set(hovered.filter((token) => !token.startsWith("column:")));
    cy.batch(() =>
      cy.elements().forEach((element) => {
        const concept = String(element.data("concept") ?? "").split(" ");
        element.toggleClass("lit", concept.some((token) => lit.has(token)));
      }),
    );
  }, [hovered, trace, wave.from, wave.to, wave.unit, reduced, rowEdges]);

  return <div ref={container} className="h-full min-h-[420px] w-full" />;
};
