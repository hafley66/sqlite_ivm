import { animate, useMotionValue, useMotionValueEvent, useReducedMotion, type AnimationPlaybackControls, type MotionValue } from "motion/react";
import { createContext, useContext, useEffect, useMemo, useRef, useState } from "react";
import type { Change, Trace } from "./0_trace";
import { emitted, isEntity, tokens } from "./1_labels";
import { quote, useHover } from "./2_hover";

// A step change travels through the page as a wave: one unit per dependency depth of the IR graph.
// Forward:  story (unit 0) → graph nodes by depth (1 + depth; the inspector follows its node) → answer → engines.
// Backward: answer (unit 0) → graph nodes, deepest first → story → engines.
// Until the wave reaches a stage, that stage keeps showing the step it showed before.

// Seconds per wave unit at 1×.
export const UNIT_SECONDS = 1;

export type Speed = 0.25 | 0.5 | 1 | 2 | 4 | "step";
export const SPEEDS: Speed[] = [0.25, 0.5, 1, 2, 4, "step"];

export type Stage = "story" | "answer" | "engines" | { node: number };

export type WaveControls = {
  toggle: () => void;            // play / pause; replays the last wave when it has finished
  jump: (unit: number) => void;  // move the wave to the start of `unit` and pause there
  advance: (stops: number[]) => void; // to the next stop (sub-step) after the clock, paused
};

export type Wave = {
  from: number;          // step shown by stages the wave has not reached
  to: number;            // step shown by stages the wave has reached (from = to before the first step change)
  unit: number;          // current wave position; `end` when finished
  end: number;
  depth: Map<number, number>;
  max_depth: number;
  clock: MotionValue<number>; // continuous wave position in units, 0 → end
  playing: boolean;
  speed: Speed;
  controls: WaveControls;
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

type WaveShape = Pick<Wave, "from" | "to" | "unit" | "depth" | "max_depth">;

export const stageUnit = (wave: WaveShape, stage: Stage) => {
  const forward = wave.to >= wave.from;
  if (stage === "engines") return wave.max_depth + 3;
  if (stage === "story") return forward ? 0 : wave.max_depth + 2;
  if (stage === "answer") return forward ? wave.max_depth + 2 : 0;
  const depth = wave.depth.get(stage.node) ?? 0;
  return 1 + (forward ? depth : wave.max_depth - depth);
};

export const reached = (wave: WaveShape, stage: Stage) => wave.unit >= stageUnit(wave, stage);
export const isActive = (wave: WaveShape, stage: Stage) => wave.from !== wave.to && wave.unit === stageUnit(wave, stage);
export const shownIndex = (wave: WaveShape, stage: Stage) => (reached(wave, stage) ? wave.to : wave.from);

// Wall-clock seconds of one unit at the current speed; step-through uses 1×.
export const unitSeconds = (wave: Pick<Wave, "speed">) => UNIT_SECONDS / (wave.speed === "step" ? 1 : wave.speed);

export const WaveContext = createContext<Wave>(null as unknown as Wave);
export const useWave = () => useContext(WaveContext);

// The wave clock is a motion value tweened from 0 to `end`; the tween's playback controls give
// pause, play, speed and seeking. A new step stops the running tween; the new wave starts from
// the step the old one was heading to.
export const useWaveState = (trace: Trace, stepIndex: number, speed: Speed, reduced: boolean): Wave => {
  const { depth, max_depth } = useMemo(() => depths(trace), [trace]);
  const end = max_depth + 4;
  const [state, setState] = useState({ from: stepIndex, to: stepIndex, unit: end });
  const [playing, setPlaying] = useState(false);
  const clock = useMotionValue(end);
  const tween = useRef<AnimationPlaybackControls | null>(null);
  const target = useRef(stepIndex);
  const speedRef = useRef(speed);
  speedRef.current = speed;

  useMotionValueEvent(clock, "change", (value) => {
    const unit = Math.min(end, Math.floor(value + 1e-6));
    setState((current) => (current.unit === unit ? current : { ...current, unit }));
  });

  const run = (start: number, paused: boolean) => {
    tween.current?.stop();
    if (reduced) {
      clock.set(end);
      setPlaying(false);
      return;
    }
    clock.set(start);
    const controls = animate(clock, end, {
      duration: (end - start) * UNIT_SECONDS,
      ease: "linear",
      onComplete: () => setPlaying(false),
    });
    controls.speed = speedRef.current === "step" ? 1 : speedRef.current;
    if (paused) controls.pause();
    tween.current = controls;
    setPlaying(!paused);
  };

  useEffect(() => {
    const from = target.current;
    target.current = stepIndex;
    if (from === stepIndex) return;
    setState({ from, to: stepIndex, unit: 0 });
    run(0, speedRef.current === "step");
  }, [stepIndex]);

  useEffect(() => () => tween.current?.stop(), []);

  // Speed changes apply to the running tween; entering step-through pauses it.
  useEffect(() => {
    const controls = tween.current;
    if (!controls) return;
    if (speed === "step") {
      controls.pause();
      setPlaying(false);
    } else {
      controls.speed = speed;
    }
  }, [speed]);

  const controls: WaveControls = {
    toggle: () => {
      const running = tween.current;
      if (!running || clock.get() >= end) return run(0, false);
      if (playing) {
        running.pause();
        setPlaying(false);
      } else {
        if (speedRef.current === "step") return;
        running.play();
        setPlaying(true);
      }
    },
    jump: (unit) => run(Math.max(0, Math.min(end, unit)), true),
    advance: (stops) => {
      const now = clock.get();
      if (now >= end) return;
      const next = stops.find((stop) => stop > now + 1e-6) ?? end;
      run(next, true);
    },
  };

  return { ...state, end, depth, max_depth, clock, playing, speed, controls };
};

// ---- pulse: the wave's own linked highlight ----
// When the wave reaches a stage, the tokens of the rows changing there pulse everywhere they are
// mentioned, like hover. `id` changes every unit so the keyframes restart.

export type Pulse = { id: string; plus: string[]; minus: string[]; node: string[] };

const signedTokens = (names: Trace["names"], changes: Change[], pulse: Pulse) => {
  for (const change of changes) {
    const bucket = change.w > 0 ? pulse.plus : pulse.minus;
    bucket.push(tokens.row(change.row), ...change.row.filter((value) => isEntity(names, value)).map(tokens.entity));
  }
};

export const pulseOf = (trace: Trace, wave: Wave): Pulse => {
  const pulse: Pulse = { id: `${wave.from}-${wave.to}-u${wave.unit + 1}`, plus: [], minus: [], node: [] };
  if (wave.from === wave.to) return pulse;
  const step = trace.steps[wave.to];
  if (!step) return pulse;
  const outputs = (delta: typeof step.oracle) => delta.map(({ row, w }) => ({ row, w }));
  if (isActive(wave, "story")) signedTokens(trace.names, outputs(step.frontier), pulse);
  for (const node of trace.nodes) {
    if (!isActive(wave, { node: node.id })) continue;
    const changes = emitted(step, node.id);
    if (changes.length === 0) continue;
    pulse.node.push(tokens.node(node.id));
    signedTokens(trace.names, changes, pulse);
  }
  if (isActive(wave, "answer")) signedTokens(trace.names, outputs(step.oracle), pulse);
  if (isActive(wave, "engines")) signedTokens(trace.names, outputs(step.dd), pulse);
  // A token both added and removed in one stage (an entity in two rows) pulses as removed.
  pulse.plus = [...new Set(pulse.plus)].filter((token) => !pulse.minus.includes(token));
  pulse.minus = [...new Set(pulse.minus)];
  pulse.node = [...new Set(pulse.node)];
  return pulse;
};


// Entity tokens pulse only the name itself (an element whose whole concept is that entity), so a
// row that merely mentions Red does not flash; row and node tokens pulse every element carrying them.
const pulseSelector = (token: string) => (token.startsWith("entity:") ? `[data-concept=${quote(token)}]` : `[data-concept~=${quote(token)}]`);

// Same pattern as HoverStyle: rules only for the pulsed tokens; hovered elements are excluded so hover wins.
export const PulseStyle = ({ trace }: { trace: Trace }) => {
  const wave = useWave();
  const hovered = useHover();
  const reduced = useReducedMotion();
  if (reduced) return null;
  const pulse = pulseOf(trace, wave);
  const notHovered = hovered
    .filter((token) => !token.startsWith("column:"))
    .map((token) => `:not([data-concept~=${quote(token)}])`)
    .join("");
  const tones = [
    { name: "plus", list: pulse.plus, color: "16 185 129" },
    { name: "minus", list: pulse.minus, color: "244 63 94" },
    { name: "node", list: pulse.node, color: "14 165 233" },
  ];
  const rules = tones
    .filter((tone) => tone.list.length > 0)
    .map((tone) => {
      const animation = `pulse-${tone.name}-${pulse.id}`;
      return [
        `@keyframes ${animation}{0%{background-color:rgb(${tone.color}/.45);box-shadow:0 0 0 3px rgb(${tone.color}/.8)}100%{background-color:rgb(${tone.color}/0);box-shadow:0 0 0 3px rgb(${tone.color}/0)}}`,
        `${tone.list.map((token) => `${pulseSelector(token)}${notHovered}`).join(",")}{animation:${animation} ${Math.round(unitSeconds(wave) * 800)}ms ease-out;border-radius:4px}`,
      ].join("\n");
    })
    .join("\n");
  return <style>{rules}</style>;
};
