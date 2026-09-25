import cytoscape, { type Core, type EdgeSingular, type StylesheetJson } from "cytoscape";
import dagre from "cytoscape-dagre";
import { useEffect, useRef } from "react";
import { animate } from "motion/react";
import type { Change, Trace } from "./0_trace";
import { emitted, isEntity, nodeConcept, nodeTitle, opWord, rowKey, rowText, tokens } from "./1_labels";
import { hover, useHover } from "./2_hover";
import { isActive, pulseOf, shownIndex, unitSeconds, useWave } from "./3a_wave";

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

// Row edges drawn per base edge before the rest collapse into one "+N more rows" edge.
const ROW_CAP = 8;

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
  { selector: "edge.marching", style: { width: 4, "line-style": "dashed", "line-dash-pattern": [9, 5], "line-color": "#0ea5e9", "target-arrow-color": "#0ea5e9" } },
  // "one line per row": row edges fan out beside the base edge.
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
  const container = useRef<HTMLDivElement>(null);
  const cyRef = useRef<Core | null>(null);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const hovered = useHover();
  // Per base edge: the step whose label it shows, and the running label tween.
  const edgeShown = useRef(new Map<string, number>());
  const labelTweens = useRef(new Map<string, { stop: () => void }>());

  // Build once per trace; layout is stable across steps.
  useEffect(() => {
    const anyLoop = trace.nodes.some((node) => node.in_loop);
    const cy = cytoscape({
      container: container.current,
      style: stylesheet(reduced),
      wheelSensitivity: 0.3,
      maxZoom: 1.25,
      elements: [
        ...(anyLoop ? [{ data: { id: loopParent, label: "repeats until nothing changes", self: "", concept: "" } }] : []),
        ...trace.nodes.map((node) => ({
          data: {
            id: `n${node.id}`,
            node: node.id,
            label: (node.caption || node.relation !== null) ? `${nodeTitle(trace, node)}\n(${opWord(node.op)})` : nodeTitle(trace, node),
            parent: node.in_loop ? loopParent : undefined,
            self: nodeConcept(trace, node),
            concept: nodeConcept(trace, node),
          },
        })),
        ...trace.nodes.flatMap((node) =>
          node.inputs.map((input, index) => ({
            data: { id: `e${input}-${node.id}-${index}`, source: `n${input}`, target: `n${node.id}`, input, node: node.id, label: "", self: "", concept: "" },
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
      cy.fit(cy.nodes(), 16);
    });
    resized.observe(container.current!);
    return () => {
      resized.disconnect();
      labelTweens.current.forEach((tween) => tween.stop());
      labelTweens.current.clear();
      cy.destroy();
      cyRef.current = null;
    };
  }, [trace, reduced]);

  // A new wave: finish every running dash march, row fade and label tween so nothing stale stays lit.
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    return () => {
      if (cy.destroyed()) return;
      cy.edges().filter((edge) => edge.data("leaving") === true).remove();
      cy.edges().stop(true, false).removeClass("marching").removeStyle("line-dash-offset opacity width");
      labelTweens.current.forEach((tween) => tween.stop());
      labelTweens.current.clear();
    };
  }, [trace, wave.from, wave.to]);

  // Each wave unit: nodes and edges the wave has reached switch to the new step; the active depth pulses.
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    const unitMs = unitSeconds(wave) * 1000;

    const leave = (edge: EdgeSingular) => {
      edge.data("leaving", true);
      if (reduced) return void cy.remove(edge);
      edge.stop(true, false);
      edge.animate({ style: { opacity: 0, width: 0.2 } }, { duration: unitMs, easing: "ease-in", complete: () => void (edge.removed() || cy.remove(edge)) });
    };
    const enter = (edge: EdgeSingular) => {
      if (reduced) return;
      edge.style({ opacity: 0, width: 0 });
      edge.animate({ style: { opacity: 1, width: 2.5 } }, { duration: unitMs * 0.6, complete: () => void edge.removeStyle("opacity width") });
    };
    // One edge per changed row on this base edge, keyed by (source, target, row token).
    const syncRows = (base: EdgeSingular, changes: Change[], columns: string[]) => {
      const baseId = base.id();
      const shown = rowEdges ? changes.slice(0, ROW_CAP) : [];
      const hidden = rowEdges ? changes.slice(ROW_CAP) : [];
      const want = new Map(shown.map((change) => [`${baseId}|${rowKey(change.row)}`, change]));
      cy.edges(".row").forEach((edge) => {
        if (edge.data("base") === baseId && !edge.data("leaving") && !want.has(edge.id())) leave(edge);
      });
      for (const [id, change] of want) {
        let edge = cy.getElementById(id) as unknown as EdgeSingular;
        if (edge.nonempty() && edge.data("leaving")) {
          edge.stop(true, false);
          cy.remove(edge);
          edge = cy.collection() as unknown as EdgeSingular;
        }
        if (edge.empty()) {
          const concept = changeTokens(trace.names, [change]).join(" ");
          edge = cy.add({
            group: "edges",
            data: { id, source: base.data("source"), target: base.data("target"), base: baseId, concept, self: concept, rowlabel: `${change.w > 0 ? "+" : "−"} ${rowText(trace.names, change.row, columns)}` },
            classes: "row",
          }) as unknown as EdgeSingular;
          enter(edge);
        }
        edge.toggleClass("row-plus", change.w > 0);
        edge.toggleClass("row-minus", change.w < 0);
      }
      const moreId = `${baseId}|more`;
      const more = cy.getElementById(moreId);
      if (hidden.length > 0) {
        const label = `+${hidden.length} more rows`;
        const concept = changeTokens(trace.names, hidden).join(" ");
        if (more.empty() || more.data("leaving")) {
          if (more.nonempty()) cy.remove(more);
          enter(cy.add({ group: "edges", data: { id: moreId, source: base.data("source"), target: base.data("target"), base: baseId, label, concept }, classes: "more" }) as unknown as EdgeSingular);
        } else {
          more.data({ label, concept });
        }
      } else if (more.nonempty() && !more.data("leaving")) {
        leave(more as unknown as EdgeSingular);
      }
    };

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
      // An edge carries its source node's output, so it changes, marches and pulses at the source's stage.
      const input = edge.data("input") as number;
      const stage = { node: input };
      const shown = shownIndex(wave, stage);
      const changes = emitted(trace.steps[shown], input);
      edge.data("concept", changeTokens(trace.names, changes).join(" "));
      edge.toggleClass("flowing", changes.length > 0);
      edge.toggleClass("thin", rowEdges);
      syncRows(edge, changes, trace.nodes.find((node) => node.id === input)?.columns ?? []);

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

      // Marching dash while the source's depth is active: on the row edges when they are drawn.
      const marchers = rowEdges ? cy.edges(".row").filter((row) => row.data("base") === edge.id() && !row.data("leaving")) : edge;
      if (!reduced && isActive(wave, stage) && changes.length > 0) {
        marchers.forEach((marcher) => {
          if (marcher.hasClass("marching")) return;
          marcher.addClass("marching");
          marcher.animate(
            { style: { "line-dash-offset": -42 } },
            {
              duration: unitMs * 2,
              easing: "linear",
              queue: false,
              complete: () => {
                marcher.removeClass("marching");
                marcher.removeStyle("line-dash-offset");
              },
            },
          );
        });
      }
    });

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
  }, [trace, wave.from, wave.to, wave.unit, wave.speed, reduced, rowEdges]);

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
