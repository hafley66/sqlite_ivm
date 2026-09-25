import type { Trace } from "./0_trace";
import { tokens } from "./1_labels";

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

export const Scrubber = ({ trace, stepIndex, onStep }: { trace: Trace; stepIndex: number; onStep: (index: number) => void }) => {
  const last = trace.steps.length - 1;
  const step = trace.steps[stepIndex];
  return (
    <div className="flex items-center gap-3 rounded-b-md rounded-tr-md border border-slate-300 bg-white px-3 py-2">
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
      <div className="ml-auto text-xs text-slate-400">← → keys step</div>
    </div>
  );
};
