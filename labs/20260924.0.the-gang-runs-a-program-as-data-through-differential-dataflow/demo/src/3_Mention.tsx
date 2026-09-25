import { createContext, useContext, type ReactNode } from "react";
import type { Change, Row, Trace, TraceNode } from "./0_trace";
import { cells, nodeConcept, nodeTitle, reasons, rowConcept, tokens } from "./1_labels";

export const TraceContext = createContext<Trace>(null as unknown as Trace);
export const useTrace = () => useContext(TraceContext);

export const Entity = ({ value }: { value: number }) => {
  const trace = useTrace();
  return (
    <span data-concept={tokens.entity(value)} className="font-semibold text-indigo-800">
      {trace.names[String(value)]}
    </span>
  );
};

// "Red → Surf", "Red, level 34": entities by name joined by arrows, plain numbers with their column name.
export const RowSentence = ({ row, columns }: { row: Row; columns: string[] }) => {
  const trace = useTrace();
  return (
    <span data-concept={rowConcept(trace.names, row, columns)} className="px-0.5">
      {cells(trace.names, row, columns).map((cell, index) => (
        <span key={index}>
          {index > 0 && <span className="text-slate-400">{cell.kind === "entity" ? " → " : ", "}</span>}
          {cell.kind === "entity" ? (
            <Entity value={cell.value} />
          ) : (
            <span>
              <span data-concept={tokens.column(cell.column)} className="text-slate-500">{cell.column.replaceAll("_", " ")}</span>{" "}
              <span className="font-mono">{cell.value}</span>
            </span>
          )}
        </span>
      ))}
    </span>
  );
};

export const Chip = ({ children, tone = "slate", concept }: { children: ReactNode; tone?: "slate" | "green" | "red" | "amber"; concept?: string }) => {
  const tones = {
    slate: "bg-slate-100 text-slate-700",
    green: "bg-emerald-100 text-emerald-800",
    red: "bg-rose-100 text-rose-800",
    amber: "bg-amber-100 text-amber-800",
  };
  return (
    <span data-concept={concept} className={`ml-1 inline-block rounded-full px-2 py-px text-xs whitespace-nowrap ${tones[tone]}`}>
      {children}
    </span>
  );
};

export const ReasonsChip = ({ w }: { w: number }) => <Chip>{reasons(w)}</Chip>;

// "+ Red → Surf" / "− Red → Surf", with a count chip when the change is more than one copy.
export const ChangeLine = ({ change, columns, suffix }: { change: Change; columns: string[]; suffix?: ReactNode }) => (
  <div className="flex items-baseline gap-1 py-0.5">
    <span className={`w-4 shrink-0 text-center font-bold ${change.w > 0 ? "text-emerald-600" : "text-rose-600"}`}>
      {change.w > 0 ? "+" : "−"}
    </span>
    <span>
      <RowSentence row={change.row} columns={columns} />
      {Math.abs(change.w) !== 1 && <ReasonsChip w={Math.abs(change.w)} />}
      {suffix}
    </span>
  </div>
);

export const ChangeList = ({ changes, columns, empty = "nothing" }: { changes: Change[]; columns: string[]; empty?: string }) =>
  changes.length === 0 ? (
    <div className="text-sm text-slate-400 italic">{empty}</div>
  ) : (
    <div className="text-sm">
      {changes.map((change, index) => (
        <ChangeLine key={index} change={change} columns={columns} />
      ))}
    </div>
  );

export const NodeName = ({ node }: { node: TraceNode }) => {
  const trace = useTrace();
  return (
    <span data-concept={nodeConcept(trace, node)} className="font-medium text-sky-800">
      {nodeTitle(trace, node)}
    </span>
  );
};

export const RelName = ({ relation }: { relation: number }) => {
  const trace = useTrace();
  const found = trace.relations.find((candidate) => candidate.id === relation);
  return (
    <span data-concept={found ? tokens.rel(found.name) : undefined} className="font-medium text-teal-800">
      {found?.name ?? "?"}
    </span>
  );
};

export const Section = ({ title, children, className = "" }: { title: ReactNode; children: ReactNode; className?: string }) => (
  <section className={`rounded-lg border border-slate-200 bg-white p-3 shadow-sm ${className}`}>
    <h2 className="mb-2 text-xs font-semibold tracking-wide text-slate-500 uppercase">{title}</h2>
    {children}
  </section>
);
