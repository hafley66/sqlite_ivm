import cytoscape, { type Core, type StylesheetJson } from "cytoscape";
import dagre from "cytoscape-dagre";
import { useEffect, useRef } from "react";
import { animate } from "motion/react";
import type { Change, Trace, TraceStep } from "./0_trace";
import { consolidate, isEntity, nodeConcept, nodeTitle, opWord, tokens } from "./1_labels";
import { hover, useHover } from "./2_hover";
import { isActive, shownIndex, UNIT_SECONDS, useWave } from "./3a_wave";

cytoscape.use(dagre);

const loopParent = "loop";

// The changes a node emitted this step: the SQLite delta table, or the dd changes when SQLite kept none.
export const emitted = (step: TraceStep | undefined, id: number): Change[] => {
  const stepNode = step?.nodes.find((candidate) => candidate.id === id);
  if (!stepNode) return [];
  return consolidate(stepNode.sqlite_changes.length ? stepNode.sqlite_changes : stepNode.dd_changes);
};

const changeTokens = (names: Trace["names"], changes: Change[]) =>
  changes.flatMap((change) => [tokens.row(change.row), ...change.row.filter((value) => isEntity(names, value)).map(tokens.entity)]);


type Counts = { added: number; removed: number };

const counts = (changes: Change[]): Counts => ({
  added: changes.filter((change) => change.w > 0).length,
  removed: changes.filter((change) => change.w < 0).length,
});

const edgeLabel = ({ added, removed }: Counts) => [added ? `+${added}` : "", removed ? `−${removed}` : ""].filter(Boolean).join(" ");

const MARCH_MS = UNIT_SECONDS * 1000 * 2;

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
  { selector: "edge.lit", style: { width: 5, "line-color": "#eab308", "target-arrow-color": "#eab308", "text-background-color": "#fde047" } },
];

export const Graph = ({ trace, selected, onSelect, reduced }: { trace: Trace; selected: number | null; onSelect: (id: number) => void; reduced: boolean }) => {
  const wave = useWave();
  const container = useRef<HTMLDivElement>(null);
  const cyRef = useRef<Core | null>(null);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const hovered = useHover();
  // Per edge: the step whose label it shows, and the running label tween.
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
      cy.fit(undefined, 16);
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

  // A new wave: finish every running dash march and label tween so nothing stale stays lit.
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    return () => {
      if (cy.destroyed()) return;
      cy.edges().stop(true, false).removeClass("marching").removeStyle("line-dash-offset");
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
    cy.edges().forEach((edge) => {
      const stage = { node: edge.data("node") as number };
      const shown = shownIndex(wave, stage);
      const input = edge.data("input") as number;
      const changes = emitted(trace.steps[shown], input);
      edge.data("concept", changeTokens(trace.names, changes).join(" "));
      edge.toggleClass("flowing", changes.length > 0);

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
            duration: UNIT_SECONDS * 1.5,
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

      if (!reduced && isActive(wave, stage) && changes.length > 0 && !edge.hasClass("marching")) {
        edge.addClass("marching");
        edge.animate(
          { style: { "line-dash-offset": -42 } },
          {
            duration: MARCH_MS,
            easing: "linear",
            complete: () => {
              edge.removeClass("marching");
              edge.removeStyle("line-dash-offset");
            },
          },
        );
      }
    });
  }, [trace, wave, reduced]);

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
  }, [hovered, trace, wave, reduced]);

  return <div ref={container} className="h-full min-h-[420px] w-full" />;
};
