import { motion, useTransform } from "motion/react";
import type { ReactNode } from "react";
import type { Trace } from "./0_trace";
import { nodeTitle, tokens } from "./1_labels";
import { SPEEDS, stageUnit, unitSeconds, useWave, type Speed, type Wave } from "./3a_wave";

export const ScenarioTabs = ({ traces, current, onPick }: { traces: Trace[]; current: string; onPick: (scenario: string) => void }) => (
  <nav className="flex flex-wrap gap-1">
    {traces.map((trace) => (
      <button
        key={trace.scenario}
        onClick={() => onPick(trace.scenario)}
        className={`rounded-t-md border border-b-0 px-3 py-1.5 text-sm ${trace.scenario === current ? "border-slate-300 bg-white font-semibold" : "border-transparent bg-slate-200 text-slate-600 hover:bg-slate-100"}`}
      >
        {trace.title}
      </button>
    ))}
  </nav>
);

export const Scrubber = ({ trace, stepIndex, onStep, settings }: { trace: Trace; stepIndex: number; onStep: (index: number) => void; settings: ReactNode }) => {
  const last = trace.steps.length - 1;
  const step = trace.steps[stepIndex];
  return (
    <div className="flex items-center gap-3 rounded-tr-md border border-slate-300 bg-white px-3 py-2">
      <button className="rounded border px-2 py-0.5 disabled:opacity-30" disabled={stepIndex <= 0} onClick={() => onStep(stepIndex - 1)}>
        ← prev
      </button>
      <div className="flex items-center gap-1.5">
        {trace.steps.map((past) => (
          <button
            key={past.index}
            data-concept={tokens.step(past.index)}
            title={past.caption}
            onClick={() => onStep(past.index)}
            className={`h-3.5 w-3.5 rounded-full border ${past.index === stepIndex ? "border-sky-700 bg-sky-600" : past.index < stepIndex ? "border-sky-400 bg-sky-200" : "border-slate-300 bg-white"} ${past.error ? "ring-1 ring-rose-400" : ""}`}
          />
        ))}
      </div>
      <button className="rounded border px-2 py-0.5 disabled:opacity-30" disabled={stepIndex >= last} onClick={() => onStep(stepIndex + 1)}>
        next →
      </button>
      {step && (
        <div data-concept={tokens.step(step.index)} className="text-sm">
          <span className="text-slate-500">
            Step {step.index + 1} of {trace.steps.length}:
          </span>{" "}
          <span className="font-medium">{step.caption}</span>
        </div>
      )}
      <div className="ml-auto flex items-center gap-3">
        {settings}
        <div className="text-xs text-slate-400">← → step · space play/pause or next stage</div>
      </div>
    </div>
  );
};

// ---- wave timer: progress, stage ticks, play/pause, speed ----

const stagesAt = (trace: Trace, wave: Wave, unit: number) => {
  const nodes = trace.nodes.filter((node) => stageUnit(wave, { node: node.id }) === unit);
  const named = (["story", "answer", "engines"] as const).filter((stage) => stageUnit(wave, stage) === unit);
  return { nodes, named };
};

const stageWords = { story: "Story", answer: "Answer", engines: "Engines" } as const;

export const WaveBar = ({ trace }: { trace: Trace }) => {
  const wave = useWave();
  const seconds = unitSeconds(wave);
  const width = useTransform(wave.clock, (value) => `${(Math.min(value, wave.end) / wave.end) * 100}%`);
  const elapsed = useTransform(wave.clock, (value) => `${(Math.min(value, wave.end) * seconds).toFixed(1)} s / ${(wave.end * seconds).toFixed(1)} s`);
  const units = Array.from({ length: wave.end }, (_, unit) => unit);
  return (
    <div className="flex items-center gap-3 border-x border-b border-slate-300 bg-white px-3 py-1.5 text-xs">
      <button className="w-20 rounded border px-2 py-0.5 whitespace-nowrap" onClick={wave.controls.toggle} disabled={wave.speed === "step"}>
        {wave.playing ? "❚❚ pause" : "▶ play"}
      </button>
      {wave.speed === "step" && (
        <button className="rounded border border-sky-400 px-2 py-0.5 whitespace-nowrap text-sky-800" onClick={wave.controls.advance}>
          next stage ⏎
        </button>
      )}
      <div className="relative h-7 flex-1 overflow-hidden rounded bg-slate-100">
        <motion.div className="absolute inset-y-0 left-0 bg-sky-200/70" style={{ width }} />
        <div className="absolute inset-0 flex">
          {units.map((unit) => {
            const { nodes, named } = stagesAt(trace, wave, unit);
            const depth = nodes.length ? (wave.depth.get(nodes[0].id) ?? 0) + 1 : null;
            const label = named.length ? named.map((stage) => stageWords[stage]).join(" · ") : `depth ${depth}`;
            const current = wave.unit === unit && wave.unit < wave.end;
            return (
              <button
                key={unit}
                data-concept={nodes.map((node) => tokens.node(node.id)).join(" ") || undefined}
                title={nodes.map((node) => nodeTitle(trace, node)).join("\n") || label}
                onClick={() => wave.controls.jump(unit)}
                className={`relative flex-1 border-l border-slate-300 px-1 text-left whitespace-nowrap first:border-l-0 ${current ? "bg-sky-500/30 font-semibold text-sky-900" : "text-slate-600 hover:bg-slate-200/60"}`}
              >
                {label}
              </button>
            );
          })}
        </div>
      </div>
      <motion.span key={String(wave.speed)} className="w-24 text-right font-mono text-slate-600">{elapsed}</motion.span>
    </div>
  );
};

export const Settings = ({ speed, onSpeed, rowEdges, onRowEdges }: { speed: Speed; onSpeed: (speed: Speed) => void; rowEdges: boolean; onRowEdges: (on: boolean) => void }) => (
  <div className="flex items-center gap-3 text-xs text-slate-600">
    <div className="flex overflow-hidden rounded border border-slate-300">
      {SPEEDS.map((option) => (
        <button
          key={String(option)}
          onClick={() => onSpeed(option)}
          className={`border-l border-slate-300 px-2 py-0.5 first:border-l-0 ${option === speed ? "bg-sky-600 text-white" : "bg-white hover:bg-slate-100"}`}
        >
          {option === "step" ? "step through" : `${option}×`}
        </button>
      ))}
    </div>
    <label className="flex items-center gap-1">
      <input type="checkbox" checked={rowEdges} onChange={(event) => onRowEdges(event.target.checked)} />
      one line per row
    </label>
  </div>
);
