import { readFile, writeFile } from "node:fs/promises";

const inputPath = process.argv[2] ?? "results/crossover-full.jsonl";
const pairOutputPath = process.argv[3] ?? "results/crossover-summary.tsv";
const familyOutputPath = process.argv[4] ?? "results/crossover-family-summary.tsv";
const threeWayOutputPath = process.argv[5];
const records = (await readFile(inputPath, "utf8")).trim().split("\n").filter(Boolean).map(JSON.parse);

function median(values) {
  if (values.length === 0) return null;
  const sorted = values.toSorted((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0 ? (sorted[middle - 1] + sorted[middle]) / 2 : sorted[middle];
}

function number(values, operation = median) {
  const filtered = values.filter((entry) => Number.isFinite(entry));
  if (filtered.length === 0) return "";
  const value = operation === median ? median(filtered) : operation(...filtered);
  return value === null || value === undefined ? "" : value.toFixed(6);
}

function cellKey(record) {
  return `${record.circuit ?? "aggregate"}:${record.budget}:${record.fanout}:${record.rows}:${record.batch_size}`;
}

function groupBy(source, keyFor) {
  const groups = new Map();
  for (const record of source) {
    const key = keyFor(record);
    const group = groups.get(key) ?? [];
    group.push(record);
    groups.set(key, group);
  }
  return groups;
}

const planned = new Map();
for (const metadata of records.filter((record) => record.event === "run-metadata")) {
  for (const testCase of metadata.cases) planned.set(cellKey({ ...testCase, budget: metadata.budget }), { ...testCase, budget: metadata.budget });
}

const pairGroups = groupBy(records.filter((record) => record.event === "paired-run"), cellKey);
const pairColumns = [
  "budget", "fanout", "rows", "batch_size", "stage", "status", "measured_pairs",
  "full_query_update_plus_query_median_ms", "ivm_update_plus_query_median_ms",
  "full_query_over_ivm_speedup_median", "speedup_min", "speedup_max", "speedup_span_percent",
  "full_query_setup_median_ms", "ivm_setup_median_ms",
  "full_query_process_median_ms", "ivm_process_median_ms",
  "postgres_group_observed_peak_rss_kb", "full_query_database_median_bytes", "ivm_database_median_bytes",
];
const pairLines = [pairColumns.join("\t")];
for (const [key, testCase] of [...planned].sort(([, left], [, right]) => left.budget.localeCompare(right.budget)
  || left.fanout - right.fanout || left.rows - right.rows || left.batch_size - right.batch_size)) {
  const pairs = (pairGroups.get(key) ?? []).filter((record) => record.status === "ok" && record.checksum_match);
  const speedups = pairs.map((record) => record.full_query_over_ivm_speedup);
  const speedupMedian = median(speedups);
  const setups = records.filter((record) => record.event === "case-setup" && record.run_kind === "measured" && cellKey(record) === key);
  const processes = records.filter((record) => record.event === "case-process" && record.run_kind === "measured" && cellKey(record) === key);
  const totals = records.filter((record) => record.event === "case-total" && record.run_kind === "measured" && cellKey(record) === key);
  pairLines.push([
    testCase.budget,
    testCase.fanout,
    testCase.rows,
    testCase.batch_size,
    testCase.stage,
    pairs.length > 0 ? "measured" : "unmeasured",
    pairs.length,
    number(pairs.map((record) => record.full_query_update_plus_query_ms)),
    number(pairs.map((record) => record.ivm_update_plus_query_ms)),
    number(speedups),
    number(speedups, Math.min),
    number(speedups, Math.max),
    speedupMedian === null ? "" : (((Math.max(...speedups) - Math.min(...speedups)) / speedupMedian) * 100).toFixed(3),
    number(setups.filter((record) => record.maintenance === "query").map((record) => record.setup_ms)),
    number(setups.filter((record) => record.maintenance === "pg_ivm").map((record) => record.setup_ms)),
    number(processes.filter((record) => record.maintenance === "query").map((record) => record.process_wall_ms)),
    number(processes.filter((record) => record.maintenance === "pg_ivm").map((record) => record.process_wall_ms)),
    number(processes.map((record) => record.postgres_group_observed_peak_rss_kb), Math.max),
    number(totals.filter((record) => record.maintenance === "query").map((record) => Number(record.disk.database_bytes))),
    number(totals.filter((record) => record.maintenance === "pg_ivm").map((record) => Number(record.disk.database_bytes))),
  ].join("\t"));
}

const mutations = records.filter((record) => record.event === "mutation" && record.run_kind === "measured" && record.status === "ok");
const familyGroups = groupBy(mutations, (record) => `${cellKey(record)}:${record.maintenance}:${record.state}`);
const familyColumns = [
  "budget", "fanout", "rows", "batch_size", "maintenance", "state", "repetitions",
  "affected_rows", "join_affected_rows", "output_rows", "output_bytes",
  "update_transaction_median_ms", "update_transaction_min_ms", "update_transaction_max_ms",
  "query_compute_median_ms", "query_compute_min_ms", "query_compute_max_ms",
  "update_plus_query_median_ms", "update_plus_query_min_ms", "update_plus_query_max_ms",
  "client_transfer_median_ms", "checksum_median_ms",
];
const familyLines = [familyColumns.join("\t")];
for (const group of [...familyGroups.values()].sort((left, right) => left[0].budget.localeCompare(right[0].budget)
  || left[0].fanout - right[0].fanout || left[0].rows - right[0].rows
  || left[0].batch_size - right[0].batch_size || left[0].maintenance.localeCompare(right[0].maintenance)
  || left[0].state.localeCompare(right[0].state))) {
  const first = group[0];
  const update = group.map((record) => record.update_transaction_ms);
  const query = group.map((record) => record.query_compute_ms);
  const combined = group.map((record) => record.update_plus_query_ms);
  familyLines.push([
    first.budget, first.fanout, first.rows, first.batch_size, first.maintenance, first.state, group.length,
    first.affected_rows, first.join_affected_rows, first.output_rows, first.output_bytes,
    number(update), number(update, Math.min), number(update, Math.max),
    number(query), number(query, Math.min), number(query, Math.max),
    number(combined), number(combined, Math.min), number(combined, Math.max),
    number(group.map((record) => record.client_transfer_ms)),
    number(group.map((record) => record.checksum_ms)),
  ].join("\t"));
}

await writeFile(pairOutputPath, `${pairLines.join("\n")}\n`);
await writeFile(familyOutputPath, `${familyLines.join("\n")}\n`);
if (threeWayOutputPath) {
  const columns = ["budget", "rows", "batch_size", "fanout", "status", "successful_triples", "states_per_arm",
    "pg_ivm_median_ms", "pg_ivm_min_ms", "pg_ivm_max_ms", "sqlite_affected_group_median_ms", "sqlite_affected_group_min_ms", "sqlite_affected_group_max_ms",
    "dd_median_ms", "dd_min_ms", "dd_max_ms", "pg_ivm_over_sqlite_median_ratio", "pg_ivm_over_dd_median_ratio", "sqlite_over_dd_median_ratio", "final_input_hash", "final_checksum"];
  const groups = groupBy(records.filter((row) => row.event === "three-way-run"), cellKey);
  const lines = [columns.join("\t")];
  for (const [key, testCase] of planned) {
    const good = (groups.get(key) ?? []).filter((row) => row.status === "ok" && row.all_input_output_states_match);
    const values = ["pg_ivm_ms", "sqlite_affected_group_ms", "dd_ms"].flatMap((field) => {
      const samples = good.map((row) => row[field]);
      return [number(samples), number(samples, Math.min), number(samples, Math.max)];
    });
    lines.push([testCase.budget, testCase.rows, testCase.batch_size, testCase.fanout,
      good.length ? "measured" : "unmeasured", good.length, good[0]?.state_count_per_arm ?? "", ...values,
      number(good.map((row) => row.pg_ivm_ms / row.sqlite_affected_group_ms)),
      number(good.map((row) => row.pg_ivm_ms / row.dd_ms)),
      number(good.map((row) => row.sqlite_affected_group_ms / row.dd_ms)),
      good[0]?.final_input_hash ?? "", good[0]?.final_checksum ?? ""].join("\t"));
  }
  await writeFile(threeWayOutputPath, lines.join("\n") + "\n");
}
console.log(JSON.stringify({
  event: "crossover-summary",
  status: "ok",
  input: inputPath,
  pair_output: pairOutputPath,
  family_output: familyOutputPath,
  three_way_output: threeWayOutputPath,
  planned_cells: planned.size,
  measured_cells: [...pairGroups.values()].filter((group) => group.some((record) => record.status === "ok")).length,
  mutation_groups: familyGroups.size,
}));

if (process.argv[6]) {
  const lines=[["budget","rows","batch_size","fanout","arm","successful_trials","median_ms","min_ms","max_ms","states_per_arm","final_input_hash","final_checksum","circuit","capability"].join("\t")];
  for(const [key,cell] of planned) {
    const good=records.filter((r)=>["all-arm-run","circuit-admitted-run"].includes(r.event) && r.status==="ok" && r.all_input_output_states_match && cellKey(r)===key);
    const arms=[...new Set(records.filter((r)=>r.event==="run-metadata").flatMap((r)=>r.arms ?? []))];
    for(const arm of arms) {
      const samples=good.map((r)=>r.totals[arm]).filter(Number.isFinite);
      lines.push([cell.budget,cell.rows,cell.batch_size,cell.fanout,arm,samples.length,number(samples),number(samples,Math.min),number(samples,Math.max),samples.length ? good[0]?.state_count_per_arm ?? "" : 0,samples.length ? good[0]?.final_input_hash ?? "" : "",samples.length ? good[0]?.final_checksum ?? "" : "",cell.circuit ?? "aggregate",samples.length ? "executed" : records.find(r=>r.event==="capability" && r.maintenance===arm && cellKey(r)===key)?.status ?? "unmeasured"].join("\t"));
    }
  }
  await writeFile(process.argv[6],lines.join("\n")+"\n");
}
if (process.argv[7]) {
  const lines=[["budget","rows","batch_size","fanout","paired_trials","logged_over_disabled_median","ratio_min","ratio_max","process_wall_logged_over_disabled_median"].join("\t")];
  for(const [key,cell] of planned) {
    const good=records.filter((r)=>["all-arm-run","circuit-admitted-run"].includes(r.event) && r.status==="ok" && r.all_input_output_states_match && cellKey(r)===key && Number.isFinite(r.totals["sqlite-plugin-logged"]) && Number.isFinite(r.totals["sqlite-plugin-delta"]));
    const ratios=good.map((r)=>r.totals["sqlite-plugin-logged"]/r.totals["sqlite-plugin-delta"]);
    const processRatios=good.map((r)=> {
      const rows=records.filter((p)=>p.event==="case-process" && p.run_kind==="measured" && p.repetition===r.repetition && cellKey(p)===key);
      return rows.find((p)=>p.maintenance==="sqlite-plugin-logged")?.process_wall_ms/rows.find((p)=>p.maintenance==="sqlite-plugin-delta")?.process_wall_ms;
    });
    lines.push([cell.budget,cell.rows,cell.batch_size,cell.fanout,ratios.length,number(ratios),number(ratios,Math.min),number(ratios,Math.max),number(processRatios)].join("\t"));
  }
  await writeFile(process.argv[7],lines.join("\n")+"\n");
}
