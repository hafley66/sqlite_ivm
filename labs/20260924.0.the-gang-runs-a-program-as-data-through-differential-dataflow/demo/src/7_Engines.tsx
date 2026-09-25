import type { Trace, TraceStep } from "./0_trace";
import { consolidate, errorWords, forRelation, rowKey, sameChanges, tokens } from "./1_labels";
import { shownIndex, useWave } from "./3a_wave";
import { ChangeLine, Chip, RelName, Row, Rows, Section, useTrace } from "./3_Mention";

type Delta = TraceStep["dd"];

const sameDelta = (trace: Trace, left: Delta, right: Delta) =>
  trace.relations.every((relation) => sameChanges(forRelation(left, relation.id), forRelation(right, relation.id)));

export const stepAgrees = (trace: Trace, step: TraceStep) => sameDelta(trace, step.dd, step.oracle) && sameDelta(trace, step.sqlite, step.oracle);

const DeltaColumn = ({ title, delta, agrees }: { title: string; delta: Delta; agrees: boolean | null }) => {
  const trace = useTrace();
  const relations = trace.relations.filter((relation) => delta.some((change) => change.relation === relation.id));
  return (
    <div className="min-w-0 flex-1 rounded border border-slate-200 p-2">
      <div className="mb-1 text-xs font-semibold text-slate-600">
        {title}
        {agrees !== null && (agrees ? <Chip tone="green">✓ matches expected</Chip> : <Chip tone="red">✗ differs from expected</Chip>)}
      </div>
      <Rows>
      {relations.map((relation) => (
        <Row key={relation.id} className="text-sm">
          <div className="text-xs text-slate-500">
            <RelName relation={relation.id} />
          </div>
          <Rows>
            {consolidate(forRelation(delta, relation.id)).map((change) => (
              <Row key={rowKey(change.row)}>
                <ChangeLine change={change} columns={relation.columns} />
              </Row>
            ))}
          </Rows>
        </Row>
      ))}
      </Rows>
      {relations.length === 0 && <div className="text-sm text-slate-400 italic">no change to the answer</div>}
    </div>
  );
};

export const Engines = ({ stepIndex, onStep }: { stepIndex: number; onStep: (index: number) => void }) => {
  const trace = useTrace();
  const wave = useWave();
  const step = trace.steps[shownIndex(wave, "engines")];
  return (
    <Section
      title={
        <span className="flex flex-wrap items-center gap-1 normal-case">
          <span className="uppercase">Engines</span>
          <span className="ml-2 font-normal text-slate-400">every step:</span>
          {trace.steps.map((past) => {
            const agrees = stepAgrees(trace, past);
            return (
              <button
                key={past.index}
                data-concept={tokens.step(past.index)}
                onClick={() => onStep(past.index)}
                className={`rounded px-1.5 font-mono ${past.index === stepIndex ? "ring-2 ring-sky-500" : ""} ${agrees ? "bg-emerald-100 text-emerald-800" : "bg-rose-100 text-rose-800"}`}
              >
                {past.index + 1} {past.error ? "rejected" : agrees ? "✓" : "✗"}
              </button>
            );
          })}
        </span>
      }
    >
      {step?.error && <div className="rounded bg-rose-50 p-2 text-sm text-rose-800">rejected: {errorWords(step.error)}</div>}
      {step && !step.error && (
        <div className="flex gap-2">
          <DeltaColumn title="Differential dataflow" delta={step.dd} agrees={sameDelta(trace, step.dd, step.oracle)} />
          <DeltaColumn title="SQLite engine" delta={step.sqlite} agrees={sameDelta(trace, step.sqlite, step.oracle)} />
          <DeltaColumn title="Expected (plain SQLite view)" delta={step.oracle} agrees={null} />
        </div>
      )}
    </Section>
  );
};
