import { readFile, writeFile } from "node:fs/promises";

const inputPath = process.argv[2] ?? "results/scale.jsonl";
const outputPath = process.argv[3] ?? "results/scale-summary.tsv";
const records = (await readFile(inputPath, "utf8"))
  .trim()
  .split("\n")
  .map((line) => JSON.parse(line));

function median(values) {
  const sorted = values.toSorted((left, right) => left - right);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 === 0
    ? (sorted[middle - 1] + sorted[middle]) / 2
    : sorted[middle];
}

function measuredKey(record) {
  return `${record.rows}:${record.batch_size}:${record.arm}:${record.repetition}`;
}

const runs = new Map();
for (const record of records) {
  if (record.status !== "ok" || record.run_kind !== "measured" || !record.arm) continue;
  const key = measuredKey(record);
  const run = runs.get(key) ?? {
    rows: record.rows,
    batch_size: record.batch_size,
    arm: record.arm,
    repetition: record.repetition,
    setup_ms: 0,
    update_transaction_ms: 0,
    query_readback_ms: 0,
    client_transfer_ms: 0,
    checksum_ms: 0,
    case_wall_ms: 0,
    process_wall_ms: 0,
    pglite_js_wasm_peak_rss_kb: 0,
    native_backend_peak_rss_kb: 0,
    native_client_peak_rss_kb: 0,
  };
  if (record.event === "case-setup") {
    run.setup_ms = record.schema_ms + record.load_ms + record.index_ms + record.view_build_ms;
  }
  if (record.event === "mutation" && record.state !== "initial") {
    run.update_transaction_ms += record.update_transaction_ms;
    run.query_readback_ms += record.query_readback_ms;
    run.client_transfer_ms += record.client_transfer_ms;
    run.checksum_ms += record.checksum_ms;
    run.pglite_js_wasm_peak_rss_kb = Math.max(
      run.pglite_js_wasm_peak_rss_kb,
      record.memory.process_peak_rss_kb ?? 0,
    );
    run.native_backend_peak_rss_kb = Math.max(
      run.native_backend_peak_rss_kb,
      record.memory.backend_rss_kb ?? 0,
    );
    run.native_client_peak_rss_kb = Math.max(
      run.native_client_peak_rss_kb,
      record.memory.client_process_peak_rss_kb ?? 0,
    );
  }
  if (record.event === "case-done") run.case_wall_ms = record.wall_ms;
  if (record.event === "case-process") run.process_wall_ms = record.process_wall_ms;
  runs.set(key, run);
}

const grouped = new Map();
for (const run of runs.values()) {
  const key = `${run.rows}:${run.batch_size}:${run.arm}`;
  const group = grouped.get(key) ?? [];
  group.push(run);
  grouped.set(key, group);
}

const columns = [
  "rows",
  "batch_size",
  "arm",
  "repetitions",
  "setup_ms_median",
  "six_updates_transaction_ms_median",
  "six_queries_readback_ms_median",
  "six_client_transfers_ms_median",
  "six_checksums_ms_median",
  "case_wall_ms_median",
  "process_wall_ms_median",
  "pglite_js_wasm_peak_rss_kb_max",
  "native_backend_peak_rss_kb_max",
  "native_client_peak_rss_kb_max",
];
const lines = [columns.join("\t")];
for (const group of [...grouped.values()].toSorted((left, right) => {
  return left[0].rows - right[0].rows
    || left[0].batch_size - right[0].batch_size
    || left[0].arm.localeCompare(right[0].arm);
})) {
  const first = group[0];
  const med = (field) => median(group.map((run) => run[field])).toFixed(3);
  const max = (field) => Math.max(...group.map((run) => run[field]));
  lines.push([
    first.rows,
    first.batch_size,
    first.arm,
    group.length,
    med("setup_ms"),
    med("update_transaction_ms"),
    med("query_readback_ms"),
    med("client_transfer_ms"),
    med("checksum_ms"),
    med("case_wall_ms"),
    med("process_wall_ms"),
    max("pglite_js_wasm_peak_rss_kb"),
    max("native_backend_peak_rss_kb"),
    max("native_client_peak_rss_kb"),
  ].join("\t"));
}

await writeFile(outputPath, `${lines.join("\n")}\n`);

const statusCounts = new Map();
for (const record of records) {
  const key = `${record.event}:${record.status ?? "none"}`;
  statusCounts.set(key, (statusCounts.get(key) ?? 0) + 1);
}
console.log(JSON.stringify({
  event: "summary-written",
  input: inputPath,
  output: outputPath,
  measured_groups: grouped.size,
  status_counts: Object.fromEntries([...statusCounts].toSorted()),
}));
