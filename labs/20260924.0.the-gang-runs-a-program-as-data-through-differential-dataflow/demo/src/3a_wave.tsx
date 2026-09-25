import { animate } from "motion/react";
import { createContext, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { Trace } from "./0_trace";

// A step change travels through the page as a wave: one unit per dependency depth of the IR graph.
// Forward:  story (unit 0) → graph nodes by depth (1 + depth) → answer + inspector → engines.
// Backward: answer + inspector (unit 0) → graph nodes, deepest first → story → engines.
// Until the wave reaches a stage, that stage keeps showing the step it showed before.

export const UNIT_SECONDS = 0.3;

export type Stage = "story" | "answer" | "engines" | { node: number };

export type Wave = {
  from: number;          // step shown by stages the wave has not reached
  to: number;            // step shown by stages the wave has reached
  unit: number;          // current wave position; `end` when idle
  end: number;
  depth: Map<number, number>;
  max_depth: number;
};

// Longest path from a node with no inputs.
export const depths = (trace: Trace) => {
  const depth = new Map<number, number>();
  const visit = (id: number, seen: Set<number>): number => {
    const known = depth.get(id);
    if (known !== undefined) return known;
    if (seen.has(id)) return 0;
    seen.add(id);
    const node = trace.nodes.find((candidate) => candidate.id === id);
    const value = node && node.inputs.length ? 1 + Math.max(...node.inputs.map((input) => visit(input, seen))) : 0;
    depth.set(id, value);
    return value;
  };
  trace.nodes.forEach((node) => visit(node.id, new Set()));
  return { depth, max_depth: Math.max(0, ...depth.values()) };
};

export const stageUnit = (wave: Wave, stage: Stage) => {
  const forward = wave.to >= wave.from;
  if (stage === "engines") return wave.max_depth + 3;
  if (stage === "story") return forward ? 0 : wave.max_depth + 2;
  if (stage === "answer") return forward ? wave.max_depth + 2 : 0;
  const depth = wave.depth.get(stage.node) ?? 0;
  return 1 + (forward ? depth : wave.max_depth - depth);
};

export const reached = (wave: Wave, stage: Stage) => wave.unit >= stageUnit(wave, stage);
export const isActive = (wave: Wave, stage: Stage) => wave.from !== wave.to && wave.unit === stageUnit(wave, stage);
export const shownIndex = (wave: Wave, stage: Stage) => (reached(wave, stage) ? wave.to : wave.from);

export const WaveContext = createContext<Wave>(null as unknown as Wave);
export const useWave = () => useContext(WaveContext);

// The wave clock is a motion tween from 0 to `end`; a new step stops the running one, and the new
// wave starts from the step the old one was heading to.
export const useWaveState = (trace: Trace, stepIndex: number, reduced: boolean): Wave => {
  const { depth, max_depth } = useMemo(() => depths(trace), [trace]);
  const end = max_depth + 4;
  const [state, setState] = useState({ from: stepIndex, to: stepIndex, unit: end });
  const target = useRef(stepIndex);

  useEffect(() => {
    const from = target.current;
    target.current = stepIndex;
    if (from === stepIndex) return;
    if (reduced) {
      setState({ from: stepIndex, to: stepIndex, unit: end });
      return;
    }
    setState({ from, to: stepIndex, unit: -1 });
    const controls = animate(0, end, {
      duration: end * UNIT_SECONDS,
      ease: "linear",
      onUpdate: (value) => setState((current) => (Math.floor(value) === current.unit ? current : { ...current, unit: Math.floor(value) })),
      onComplete: () => setState({ from: stepIndex, to: stepIndex, unit: end }),
    });
    return () => controls.stop();
  }, [stepIndex, reduced, end]);

  return { ...state, end, depth, max_depth };
};
