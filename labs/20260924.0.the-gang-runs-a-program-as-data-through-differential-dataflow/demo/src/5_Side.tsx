import type { Change, Trace, TraceStep } from "./0_trace";
import { castGroups, consolidate, errorWords, forRelation, rowKey, tokens, weightOf } from "./1_labels";
import { ChangeLine, Chip, Entity, ReasonsChip, RelName, RowSentence, Section, useTrace } from "./3_Mention";

export const Cast = () => {
  const trace = useTrace();
  if (Object.keys(trace.names).length === 0) return null;
  return (
    <Section title="Cast">
      {castGroups(trace.names).map((group) => (
        <div key={group.title} className="mb-2">
          <div className="text-xs text-slate-500">{group.title}</div>
          <div className="flex flex-wrap gap-x-2 gap-y-1 text-sm">
            {group.values.map((value) => (
              <Entity key={value} value={value} />
            ))}
          </div>
        </div>
      ))}
    </Section>
  );
};

const columnsOf = (trace: Trace, relation: number) => trace.relations.find((candidate) => candidate.id === relation)?.columns ?? [];

export const Story = ({ step }: { step: TraceStep | undefined }) => {
  const trace = useTrace();
  return (
    <Section title="Story">
      <p className="mb-2 text-sm font-medium">{trace.question}</p>
      {step && (
        <div data-concept={tokens.step(step.index)} className="mb-1 text-sm text-slate-600">
          Step {step.index + 1}: {step.caption}
        </div>
      )}
      {step?.error && (
        <div className="mb-1 rounded bg-rose-50 p-2 text-sm text-rose-800">
          rejected: {errorWords(step.error)}
        </div>
      )}
      <div className="text-sm">
        {step?.frontier.map((change, index) => (
          <ChangeLine
            key={index}
            change={change}
            columns={columnsOf(trace, change.relation)}
            suffix={
              <span className="text-slate-500">
                {change.w > 0 ? " added to " : " removed from "}
                <RelName relation={change.relation} />
              </span>
            }
          />
        ))}
        {step && step.frontier.length === 0 && <div className="text-slate-400 italic">no input changes</div>}
      </div>
    </Section>
  );
};

// Current contents of each derived relation: the oracle's changes summed up to and including this step.
export const Answer = ({ stepIndex }: { stepIndex: number }) => {
  const trace = useTrace();
  const derived = trace.relations.filter((relation) => relation.kind === "Derived");
  const step = trace.steps[stepIndex];
  return (
    <Section title="Answer">
      {derived.map((relation) => {
        const history: Change[] = trace.steps.slice(0, stepIndex + 1).flatMap((past) => forRelation(past.oracle, relation.id));
        const contents = consolidate(history);
        const now = step ? consolidate(forRelation(step.oracle, relation.id)) : [];
        const gone = now.filter((change) => change.w < 0 && weightOf(contents, change.row) <= 0);
        return (
          <div key={relation.id} className="mb-2 text-sm">
            <div className="mb-1 text-xs text-slate-500">
              <RelName relation={relation.id} />
            </div>
            {contents.length === 0 && gone.length === 0 && <div className="text-slate-400 italic">empty</div>}
            {contents.map((change) => {
              const granted = now.some((candidate) => candidate.w > 0 && rowKey(candidate.row) === rowKey(change.row));
              return (
                <div key={rowKey(change.row)} className={`py-0.5 ${granted ? "rounded bg-emerald-50" : ""}`}>
                  <RowSentence row={change.row} columns={relation.columns} />
                  {change.w !== 1 && <ReasonsChip w={change.w} />}
                  {granted && <Chip tone="green" concept={tokens.event("granted")}>new this step</Chip>}
                </div>
              );
            })}
            {gone.map((change) => (
              <div key={`gone-${rowKey(change.row)}`} className="rounded bg-rose-50 py-0.5 line-through decoration-rose-400">
                <RowSentence row={change.row} columns={relation.columns} />
                <Chip tone="red" concept={tokens.event("revoked")}>gone this step</Chip>
              </div>
            ))}
          </div>
        );
      })}
    </Section>
  );
};
