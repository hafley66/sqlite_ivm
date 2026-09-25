import type { Trace, TraceStep } from "./0_trace";
import { castGroups, compareRows, consolidate, errorWords, forRelation, rowKey, tokens, weightOf } from "./1_labels";
import { shownIndex, stageUnit, unitSeconds, useWave } from "./3a_wave";
import { usePlan } from "./3b_schedule";
import { ChangeLine, Chip, Entity, ReasonsChip, RelName, Row, Rows, RowSentence, Section, useTrace } from "./3_Mention";

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

export const Story = () => {
  const trace = useTrace();
  const wave = useWave();
  const plan = usePlan();
  const step: TraceStep | undefined = trace.steps[shownIndex(wave, "story")];
  // Frontier rows enter in the order their packets leave for the graph.
  const storyStart = stageUnit(wave, "story");
  const delayOf = (relation: number, row: TraceStep["frontier"][number]["row"]) => {
    const node = trace.nodes.find((candidate) => candidate.op === "Get" && candidate.relation === relation);
    const packet = plan.groups.find((group) => group.kind === "story" && group.target === node?.id)?.packets.find((candidate) => candidate.row && rowKey(candidate.row) === rowKey(row));
    return packet ? Math.max(0, (packet.start - storyStart) * unitSeconds(wave)) : 0;
  };
  return (
    <Section title="Story">
      <p className="mb-2 text-sm font-medium">{trace.question}</p>
      {step && (
        <div data-concept={tokens.step(step.index)} className="mb-1 text-sm text-slate-600">
          Step {step.index + 1}: {step.caption}
        </div>
      )}
      {step?.error && <div className="mb-1 rounded bg-rose-50 p-2 text-sm text-rose-800">rejected: {errorWords(step.error)}</div>}
      <div className="text-sm">
        <Rows>
          {step?.frontier.map((change) => (
            <Row key={`${change.relation}:${rowKey(change.row)}`} delay={delayOf(change.relation, change.row)}>
              <ChangeLine
                change={change}
                columns={columnsOf(trace, change.relation)}
                suffix={
                  <span className="text-slate-500">
                    {change.w > 0 ? " added to " : " removed from "}
                    <RelName relation={change.relation} />
                  </span>
                }
              />
            </Row>
          ))}
        </Rows>
        {step && step.frontier.length === 0 && <div className="text-slate-400 italic">no input changes</div>}
      </div>
    </Section>
  );
};

// Current contents of each derived relation: the oracle's changes summed up to and including the shown step.
// A row removed this step stays in place, struck through, and leaves on the next step.
export const Answer = () => {
  const trace = useTrace();
  const wave = useWave();
  const stepIndex = shownIndex(wave, "answer");
  const derived = trace.relations.filter((relation) => relation.kind === "Derived");
  const step = trace.steps[stepIndex];
  return (
    <Section title="Answer">
      {derived.map((relation) => {
        const contents = consolidate(trace.steps.slice(0, stepIndex + 1).flatMap((past) => forRelation(past.oracle, relation.id)));
        const now = step ? consolidate(forRelation(step.oracle, relation.id)) : [];
        const gone = now.filter((change) => change.w < 0 && weightOf(contents, change.row) <= 0).map((change) => ({ row: change.row, w: 0 }));
        const rows = [...contents, ...gone].sort((left, right) => compareRows(left.row, right.row));
        return (
          <div key={relation.id} className="mb-2 text-sm">
            <div className="mb-1 text-xs text-slate-500">
              <RelName relation={relation.id} />
            </div>
            <Rows>
              {rows.map((change) => {
                const granted = now.some((candidate) => candidate.w > 0 && rowKey(candidate.row) === rowKey(change.row));
                const revoked = change.w === 0;
                return (
                  <Row
                    key={rowKey(change.row)}
                    className={`py-0.5 motion-safe:transition-colors motion-safe:duration-500 ${granted ? "bg-emerald-50" : revoked ? "bg-rose-50 line-through decoration-rose-400" : ""}`}
                  >
                    <RowSentence row={change.row} columns={relation.columns} />
                    {change.w > 1 && <ReasonsChip w={change.w} />}
                    {granted && <Chip tone="green" concept={tokens.event("granted")}>new this step</Chip>}
                    {revoked && <Chip tone="red" concept={tokens.event("revoked")}>gone this step</Chip>}
                  </Row>
                );
              })}
            </Rows>
            {rows.length === 0 && <div className="text-slate-400 italic">empty</div>}
          </div>
        );
      })}
    </Section>
  );
};
