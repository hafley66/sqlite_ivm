import cytoscape, { type Core, type StylesheetJson } from "cytoscape";
import dagre from "cytoscape-dagre";
import { useEffect, useRef } from "react";
import type { Change, Trace, TraceStep } from "./0_trace";
import { consolidate, isEntity, nodeConcept, nodeTitle, opWord, tokens } from "./1_labels";
import { hover, useHover } from "./2_hover";

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

const edgeLabel = (changes: Change[]) => {
  const added = changes.filter((change) => change.w > 0).length;
  const removed = changes.filter((change) => change.w < 0).length;
  return [added ? `+${added}` : "", removed ? `−${removed}` : ""].filter(Boolean).join(" ");
};

const stylesheet: StylesheetJson = [
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
  {
    selector: "node.changed",
    style: { "underlay-color": "#f59e0b", "underlay-opacity": 0.45, "underlay-padding": 7, "underlay-shape": "round-rectangle", "border-color": "#d97706", "border-width": 2 },
  },
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
    },
  },
  { selector: "edge.flowing", style: { width: 3, "line-color": "#f59e0b", "target-arrow-color": "#f59e0b" } },
  { selector: "edge.lit", style: { width: 5, "line-color": "#eab308", "target-arrow-color": "#eab308", "text-background-color": "#fde047" } },
];

export const Graph = ({ trace, step, selected, onSelect }: { trace: Trace; step: TraceStep | undefined; selected: number | null; onSelect: (id: number) => void }) => {
  const container = useRef<HTMLDivElement>(null);
  const cyRef = useRef<Core | null>(null);
  const onSelectRef = useRef(onSelect);
  onSelectRef.current = onSelect;
  const hovered = useHover();

  // Build once per trace; layout is stable across steps.
  useEffect(() => {
    const anyLoop = trace.nodes.some((node) => node.in_loop);
    const cy = cytoscape({
      container: container.current,
      style: stylesheet,
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
            data: { id: `e${input}-${node.id}-${index}`, source: `n${input}`, target: `n${node.id}`, input, label: "", self: "", concept: "" },
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
    const resized = new ResizeObserver(() => {
      cy.resize();
      cy.fit(undefined, 16);
    });
    resized.observe(container.current!);
    return () => {
      resized.disconnect();
      cy.destroy();
      cyRef.current = null;
    };
  }, [trace]);

  // Per step: edge labels, glowing nodes, tokens of the rows in flight.
  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    cy.batch(() => {
      for (const node of trace.nodes) {
        const changes = emitted(step, node.id);
        const element = cy.getElementById(`n${node.id}`);
        element.toggleClass("changed", changes.length > 0);
        element.data("concept", [nodeConcept(trace, node), ...changeTokens(trace.names, changes)].join(" "));
      }
      cy.edges().forEach((edge) => {
        const changes = emitted(step, edge.data("input"));
        edge.data("label", edgeLabel(changes));
        edge.data("concept", changeTokens(trace.names, changes).join(" "));
        edge.toggleClass("flowing", changes.length > 0);
      });
    });
  }, [trace, step]);

  useEffect(() => {
    const cy = cyRef.current;
    if (!cy) return;
    cy.nodes().removeClass("selected-node");
    if (selected !== null) cy.getElementById(`n${selected}`).addClass("selected-node");
  }, [trace, selected]);

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
  }, [hovered, trace, step]);

  return <div ref={container} className="h-full min-h-[420px] w-full" />;
};
