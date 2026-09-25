import { AnimatePresence, animate, motion, useMotionValue, useReducedMotion, useTransform } from "motion/react";
import { createContext, useContext, useEffect, type ReactNode } from "react";
import type { Change, Row as RowValue, Trace, TraceNode } from "./0_trace";
import { cells, nodeConcept, nodeTitle, reasons, rowConcept, rowKey, tokens } from "./1_labels";

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
export const RowSentence = ({ row, columns }: { row: RowValue; columns: string[] }) => {
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

// A number that counts from its previous value to the new one.
export const Tween = ({ value, format = String }: { value: number; format?: (value: number) => string }) => {
  const reduced = useReducedMotion();
  const current = useMotionValue(value);
  useEffect(() => {
    if (reduced) {
      current.set(value);
      return;
    }
    const controls = animate(current, value, { duration: 0.6, ease: "easeOut" });
    return () => controls.stop();
  }, [value, reduced, current]);
  const text = useTransform(current, (latest) => format(Math.round(latest)));
  return <motion.span>{text}</motion.span>;
};

export const signed = (value: number) => (value > 0 ? `+${value}` : value < 0 ? `−${-value}` : "·");

export const ReasonsChip = ({ w }: { w: number }) => (
  <Chip>
    <Tween value={w} format={reasons} />
  </Chip>
);

// Keyed rows: unchanged rows stay put, entering rows slide in with a green flash, leaving rows
// strike through, fade red and collapse, reordered rows move (motion layout / FLIP).
export const Rows = ({ children }: { children: ReactNode }) => <AnimatePresence initial={false}>{children}</AnimatePresence>;

export const Row = ({ children, className = "" }: { children: ReactNode; className?: string }) => {
  const reduced = useReducedMotion();
  const instant = { duration: 0 };
  return (
    <motion.div
      layout={reduced ? false : "position"}
      initial={{ opacity: 0, x: -12, backgroundColor: "rgba(16,185,129,0.35)" }}
      animate={{ opacity: 1, x: 0, height: "auto", backgroundColor: "rgba(16,185,129,0)", transition: reduced ? instant : { duration: 0.35, backgroundColor: { duration: 1.2 } } }}
      exit={{ opacity: 0, height: 0, color: "#e11d48", textDecorationLine: "line-through", backgroundColor: "rgba(244,63,94,0.2)", transition: reduced ? instant : { duration: 0.5, height: { delay: 0.2, duration: 0.3 } } }}
      className="overflow-hidden rounded"
    >
      <div className={className}>{children}</div>
    </motion.div>
  );
};

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

export const ChangeList = ({ changes, columns, empty = "nothing" }: { changes: Change[]; columns: string[]; empty?: string }) => (
  <div className="text-sm">
    <Rows>
      {changes.map((change) => (
        <Row key={rowKey(change.row)}>
          <ChangeLine change={change} columns={columns} />
        </Row>
      ))}
    </Rows>
    {changes.length === 0 && <div className="text-slate-400 italic">{empty}</div>}
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
