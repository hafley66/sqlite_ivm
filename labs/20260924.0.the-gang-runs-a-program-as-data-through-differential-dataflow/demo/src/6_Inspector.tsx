import type { Change, Trace, TraceNode, TraceStep } from "./0_trace";
import { consolidate, errorWords, opWord, reasons, rowConcept, rowKey, sameChanges, tokens, weightOf } from "./1_labels";
import { ChangeList, Chip, NodeName, ReasonsChip, Row, Rows, RowSentence, Section, signed, Tween, useTrace } from "./3_Mention";
import { shownIndex, useWave } from "./3a_wave";
import { emitted } from "./4_Graph";

// Loop nodes: SQLite clears their tables every round, so only the output delta is compared.
const MatchBadge = ({ same, inLoop }: { same: boolean; inLoop: boolean }) =>
  inLoop ? <Chip tone="amber">SQLite computes this inside its loop</Chip> : same ? <Chip tone="green">✓ both engines agree</Chip> : <Chip tone="red">✗ engines disagree</Chip>;

const ChangesTable = ({ sqlite, dd, columns, inLoop }: { sqlite: Change[]; dd: Change[]; columns: string[]; inLoop: boolean }) => {
  const rows = consolidate([...sqlite.map((change) => ({ row: change.row, w: 1 })), ...dd.map((change) => ({ row: change.row, w: 1 }))]);
  return (
    <div className="text-sm">
      <div className="flex text-xs text-slate-500">
        <span className="flex-1">row</span>
        <span className="w-16">SQLite</span>
        <span className="w-16">dataflow</span>
      </div>
      <Rows>
        {rows.map(({ row }) => {
          const left = weightOf(sqlite, row);
          const right = weightOf(dd, row);
          return (
            <Row key={rowKey(row)} className={`flex ${left === right || inLoop ? "" : "bg-rose-50"}`}>
              <span className="flex-1"><RowSentence row={row} columns={columns} /></span>
              <span className="w-16 font-mono"><Tween value={left} format={signed} /></span>
              <span className="w-16 font-mono"><Tween value={right} format={signed} /></span>
            </Row>
          );
        })}
      </Rows>
      {rows.length === 0 && <div className="text-slate-400 italic">nothing changed here this step</div>}
    </div>
  );
};

const Totals = ({ label, changes, columns }: { label: string; changes: Change[]; columns: string[] }) => (
  <div className="min-w-0 flex-1 text-sm">
    <div className="text-xs text-slate-500">{label}</div>
    <Rows>
      {changes.map((change) => (
        <Row key={rowKey(change.row)}>
          <RowSentence row={change.row} columns={columns} />
          {change.w !== 1 && <ReasonsChip w={change.w} />}
        </Row>
      ))}
    </Rows>
    {changes.length === 0 && <div className="text-slate-400 italic">empty</div>}
  </div>
);

// Threshold: reasons for a row = everything its input emitted so far, summed.
const ThresholdView = ({ trace, node, stepIndex }: { trace: Trace; node: TraceNode; stepIndex: number }) => {
  const input = node.inputs[0];
  if (input === undefined) return null;
  const before = consolidate(trace.steps.slice(0, stepIndex).flatMap((past) => emitted(past, input)));
  const delta = emitted(trace.steps[stepIndex], input);
  return (
    <div className="text-sm">
      <Rows>
        {delta.map((change) => {
          const was = weightOf(before, change.row);
          const now = was + change.w;
          const event = was <= 0 && now > 0 ? "granted" : was > 0 && now <= 0 ? "revoked" : null;
          return (
            <Row key={rowKey(change.row)} className="py-0.5">
              <RowSentence row={change.row} columns={node.columns} />
              <span className="text-slate-500">
                {" "}was <Tween value={was} format={reasons} /> → now <Tween value={now} format={reasons} />
              </span>
              {event && (
                <Chip tone={event === "granted" ? "green" : "red"} concept={`${tokens.event(event)} ${rowConcept(trace.names, change.row, node.columns)}`}>
                  {event === "granted" ? "granted: crosses above zero" : "revoked: drops to zero"}
                </Chip>
              )}
            </Row>
          );
        })}
      </Rows>
      {delta.length === 0 && <div className="text-slate-400 italic">no reasons changed this step</div>}
    </div>
  );
};

const Rounds = ({ rounds, columns }: { rounds: TraceStep["nodes"][number]["dd_rounds"]; columns: string[] }) => {
  const numbers = [...new Set(rounds.map((entry) => entry.round))].sort((a, b) => a - b);
  return (
    <div className="text-sm">
      {numbers.map((round) => (
        <div key={round} className="mb-1">
          <div className="text-xs text-slate-500">{round === 0 ? "coming into the loop" : `time around the loop #${round}`}</div>
          <ChangeList changes={consolidate(rounds.filter((entry) => entry.round === round))} columns={columns} />
        </div>
      ))}
    </div>
  );
};

export const EngineDrawer = ({ node }: { node: TraceNode }) => (
  <details className="mt-3 rounded border border-slate-200 bg-slate-50 p-2 text-xs">
    <summary className="cursor-pointer text-slate-600">engine view</summary>
    <div className="mt-2 space-y-2">
      <div>
        <span className="text-slate-500">IR node {node.id} · </span>
        <span className="font-mono">{node.op}</span>
      </div>
      <pre className="overflow-x-auto rounded bg-white p-2 whitespace-pre-wrap">{JSON.stringify(node.detail)}</pre>
      {node.sqlite_tables.map((table, index) => (
        <div key={table}>
          <div className="font-mono text-slate-700">{table}{index === 0 ? " (own delta)" : ""}</div>
          <pre className="overflow-x-auto rounded bg-white p-2 whitespace-pre-wrap">{node.sqlite_fill[index] ?? ""}</pre>
        </div>
      ))}
    </div>
  </details>
);

export const Inspector = ({ selected }: { selected: number | null }) => {
  const trace = useTrace();
  const wave = useWave();
  const stepIndex = shownIndex(wave, "answer");
  const node = trace.nodes.find((candidate) => candidate.id === selected);
  if (!node) {
    return (
      <Section title="Node">
        <div className="text-sm text-slate-400 italic">Click a box in the graph.</div>
      </Section>
    );
  }
  const step = trace.steps[stepIndex];
  const stepNode = step?.nodes.find((candidate) => candidate.id === node.id);
  const sqlite = consolidate(stepNode?.sqlite_changes ?? []);
  const dd = consolidate(stepNode?.dd_changes ?? []);
  const keepsTotals = trace.steps.some((past) => past.nodes.some((candidate) => candidate.id === node.id && candidate.totals_before.length > 0));
  const before = consolidate(stepNode?.totals_before ?? []);
  const inputs = node.inputs.map((input) => trace.nodes.find((candidate) => candidate.id === input)).filter((input) => input !== undefined);
  return (
    <Section title="Node">
      <div className="mb-1 text-base">
        <NodeName node={node} />
        <Chip>{opWord(node.op)}</Chip>
        {node.in_loop && <Chip tone="amber">repeats</Chip>}
      </div>
      {inputs.length > 0 && (
        <div className="mb-2 text-sm text-slate-600">
          reads from{" "}
          {inputs.map((input, index) => (
            <span key={input.id}>
              {index > 0 && ", "}
              <NodeName node={input} />
            </span>
          ))}
        </div>
      )}

      <h3 className="mt-3 mb-1 text-xs font-semibold text-slate-500">
        changes this step {!step?.error && <MatchBadge same={sameChanges(sqlite, dd)} inLoop={node.in_loop} />}
      </h3>
      {step?.error ? (
        <div className="rounded bg-rose-50 p-2 text-sm text-rose-800">rejected: {errorWords(step.error)}</div>
      ) : (
        <ChangesTable sqlite={sqlite} dd={dd} columns={node.columns} inLoop={node.in_loop} />
      )}

      {node.op === "Threshold" && (
        <>
          <h3 className="mt-3 mb-1 text-xs font-semibold text-slate-500">reasons</h3>
          <ThresholdView trace={trace} node={node} stepIndex={stepIndex} />
        </>
      )}

      {keepsTotals && (
        <>
          <h3 className="mt-3 mb-1 text-xs font-semibold text-slate-500">running totals</h3>
          <div className="flex gap-2">
            <Totals label="before" changes={before} columns={node.columns} />
            <div className="self-center text-slate-400">→</div>
            <Totals label="after" changes={consolidate([...before, ...(node.in_loop ? dd : sqlite)])} columns={node.columns} />
          </div>
        </>
      )}

      {node.op === "Join" && stepNode && stepNode.terms.length > 0 && (
        <>
          <h3 className="mt-3 mb-1 text-xs font-semibold text-slate-500">where the matches came from</h3>
          {stepNode.terms.map((term, index) => (
            <div key={index} className="mb-1">
              <div data-concept={tokens.term(index)} className="text-sm text-slate-600">{term.label}</div>
              <ChangeList changes={consolidate(term.changes)} columns={node.columns} />
            </div>
          ))}
        </>
      )}

      {stepNode && stepNode.dd_rounds.length > 0 && (
        <>
          <h3 className="mt-3 mb-1 text-xs font-semibold text-slate-500">each time around the loop</h3>
          <Rounds rounds={stepNode.dd_rounds} columns={node.columns} />
        </>
      )}

      <EngineDrawer node={node} />
    </Section>
  );
};
