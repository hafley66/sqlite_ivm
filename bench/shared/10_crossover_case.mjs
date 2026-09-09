import {
  crossoverMutationSql,
  crossoverSeed,
  crossoverStates,
  crossoverSummaryQuery,
  expectedAffectedRows,
  makeCrossoverOracle,
  validateCrossoverCase,
  crossoverInputHash,
} from "./9_crossover_workload.mjs";

function elapsedMs(started) {
  return Number(process.hrtime.bigint() - started) / 1_000_000;
}

async function timed(operation) {
  const started = process.hrtime.bigint();
  const value = await operation();
  return { ms: elapsedMs(started), value };
}

function normalizeSummary(rows) {
  const normalized = rows.map((row) => [String(row.group_id), String(row.row_count), String(row.weighted_sum)]);
  const canonical = normalized.map((row) => `S\t${row.join("\t")}`).join("\n");
  return import("node:crypto").then(({ createHash }) => ({
    checksum: createHash("sha256").update(canonical).digest("hex"),
    output_rows: normalized.length,
    output_bytes: Buffer.byteLength(canonical),
  }));
}

async function readBoundedSnapshot(adapter) {
  await adapter.exec("DROP TABLE IF EXISTS crossover_snapshot");
  const query = await timed(async () => {
    await adapter.exec(`CREATE TEMP TABLE crossover_snapshot AS SELECT group_id, row_count, weighted_sum FROM crossover_view`);
    return adapter.query("SELECT count(*)::integer AS count FROM crossover_snapshot");
  });
  const transfer = await timed(() => adapter.query(`
    SELECT group_id, row_count, weighted_sum
      FROM crossover_snapshot
     ORDER BY group_id
     LIMIT 256
  `));
  if (transfer.value.rows.length === 256) {
    const count = await adapter.query("SELECT count(*)::integer AS count FROM crossover_snapshot");
    if (count.rows[0].count > 256) throw new Error(`bounded readback exceeded: ${count.rows[0].count}`);
  }
  const checksum = await timed(() => normalizeSummary(transfer.value.rows));
  await adapter.exec("DROP TABLE crossover_snapshot");
  return {
    query_compute_ms: query.ms,
    client_transfer_ms: transfer.ms,
    checksum_ms: checksum.ms,
    ...checksum.value,
    materialized_count: query.value.rows[0].count,
    summary: transfer.value.rows.map((row) => [String(row.group_id), String(row.row_count), String(row.weighted_sum)]),
  };
}

async function createView(adapter, maintenance) {
  if (maintenance === "pg_ivm") {
    await adapter.exec("CREATE EXTENSION IF NOT EXISTS pg_ivm");
    await adapter.exec(`SELECT pgivm.create_immv('crossover_view', $$${crossoverSummaryQuery}$$)`);
    return;
  }
  await adapter.exec(`CREATE VIEW crossover_view AS ${crossoverSummaryQuery}`);
}

function planBuffers(node) {
  const totals = {};
  for (const field of [
    "Shared Hit Blocks",
    "Shared Read Blocks",
    "Shared Dirtied Blocks",
    "Shared Written Blocks",
    "Local Hit Blocks",
    "Local Read Blocks",
    "Local Dirtied Blocks",
    "Local Written Blocks",
    "Temp Read Blocks",
    "Temp Written Blocks",
  ]) totals[field] = Number(node[field] ?? 0);
  return totals;
}

async function explain(adapter, family, sql, common) {
  const result = await adapter.query(`EXPLAIN (ANALYZE, BUFFERS, WAL, FORMAT JSON) ${sql}`);
  const document = result.rows[0]["QUERY PLAN"][0];
  console.log(JSON.stringify({
    event: "diagnostic-explain",
    status: "ok",
    family,
    command: `EXPLAIN (ANALYZE, BUFFERS, WAL, FORMAT JSON) ${sql}`,
    planning_ms: document["Planning Time"],
    execution_ms: document["Execution Time"],
    buffers: planBuffers(document.Plan),
    triggers: document.Triggers ?? [],
    plan: document,
    ...common,
  }));
}

async function runDiagnostics(adapter, maintenance, mutations, common) {
  const before = await adapter.tempIo();
  await explain(adapter, "query", "SELECT group_id, row_count, weighted_sum FROM crossover_view ORDER BY group_id", common);
  await adapter.exec("BEGIN");
  try {
    await explain(adapter, "dimension_fanout_update", mutations.dimension_fanout, common);
  } finally {
    await adapter.exec("ROLLBACK");
  }
  const after = await adapter.tempIo();
  console.log(JSON.stringify({
    event: "diagnostic-temp-io",
    status: "ok",
    maintenance,
    temp_files_before: before.temp_files,
    temp_files_after: after.temp_files,
    temp_files_delta: Number(after.temp_files) - Number(before.temp_files),
    temp_bytes_before: before.temp_bytes,
    temp_bytes_after: after.temp_bytes,
    temp_bytes_delta: Number(after.temp_bytes) - Number(before.temp_bytes),
    ...common,
  }));
}

export async function runCrossoverCase(adapter, options) {
  const { maintenance, rowCount, batchSize, fanout, budget, diagnostic = false, fixture } = options;
  validateCrossoverCase(rowCount, batchSize, fanout);
  const oracle = makeCrossoverOracle(rowCount, batchSize, fanout);
  const mutations = crossoverMutationSql(rowCount, batchSize, fanout, oracle.groupCount);
  const common = {
    category: "postgres-crossover",
    engine: "native-postgres",
    maintenance,
    rows: rowCount,
    batch_size: batchSize,
    fanout,
    budget,
    seed: crossoverSeed,
  };

  const schema = await timed(() => adapter.exec(`
    CREATE TABLE dimension(group_id integer PRIMARY KEY, factor integer NOT NULL);
    CREATE TABLE fact(id integer PRIMARY KEY, group_id integer NOT NULL REFERENCES dimension(group_id), amount integer NOT NULL)
  `));
  const load = await timed(() => adapter.exec(`
    INSERT INTO dimension
    SELECT group_id, group_id % 7 + 1 FROM generate_series(0, ${oracle.groupCount - 1}) AS group_id;
    INSERT INTO fact
    SELECT id,
           CASE WHEN id <= ${fanout} THEN 0 ELSE 1 + ((id - ${fanout + 1}) % ${oracle.groupCount - 1}) END,
           (id * 37) % 1000 - 500
      FROM generate_series(1, ${rowCount}) AS id
  `));
  const indexes = await timed(() => adapter.exec("CREATE INDEX fact_group_idx ON fact(group_id)"));
  const viewBuild = await timed(() => createView(adapter, maintenance));
  const actualFanout = await adapter.query("SELECT count(*)::integer AS count FROM fact WHERE group_id = 0");
  const settings = await adapter.settings();
  const metadata = await adapter.metadata();
  const disk = await adapter.disk();
  console.log(JSON.stringify({
    event: "case-setup",
    status: "ok",
    group_count: oracle.groupCount,
    actual_join_fanout: actualFanout.rows[0].count,
    schema_ms: schema.ms,
    load_ms: load.ms,
    index_ms: indexes.ms,
    view_build_ms: viewBuild.ms,
    setup_ms: schema.ms + load.ms + indexes.ms + viewBuild.ms,
    settings,
    disk,
    ...metadata,
    ...common,
  }));

  if (diagnostic) {
    await runDiagnostics(adapter, maintenance, mutations, common);
    return;
  }

  const wallStarted = process.hrtime.bigint();
  let updateQueryTotalMs = 0;
  let finalChecksum = null;
  let finalInputHash = null;
  for (const entry of fixture?.states ?? crossoverStates.map((name) => ({ name }))) {
    const state = entry.name;
    const update = state === "initial" ? { ms: 0, value: { rowCount: 0 } } : await timed(() => adapter.transaction(entry.mutation_sql ?? mutations[state]));
    if (!fixture) oracle.apply[state]();
    const expected = entry.expected ?? oracle.snapshot();
    const actual = await readBoundedSnapshot(adapter);
    const [dimensions, facts] = await Promise.all([
      adapter.query("SELECT group_id,factor FROM dimension ORDER BY group_id"),
      adapter.query("SELECT id,group_id,amount FROM fact ORDER BY id"),
    ]);
    const observedInputs = { dimension: dimensions.rows.map((row) => [row.group_id, row.factor]),
      fact: facts.rows.map((row) => [row.id, row.group_id, row.amount]) };
    const expectedInputs = entry.inputs ?? oracle.inputRows();
    const inputHash = crossoverInputHash(observedInputs);
    const exact = JSON.stringify(observedInputs) === JSON.stringify(expectedInputs)
      && JSON.stringify(actual.summary) === JSON.stringify(expected.summary);
    const memory = await adapter.memory();
    const affectedRows = Number(update.value.rowCount ?? 0);
    const expectedAffected = entry.expected_affected_rows ?? expectedAffectedRows(state, batchSize);
    const status = actual.checksum === expected.checksum
      && actual.output_rows === expected.output_rows
      && actual.output_bytes === expected.output_bytes
      && affectedRows === expectedAffected
      && exact && actual.materialized_count === expected.output_rows
      ? "ok"
      : "mismatch";
    const updateQueryMs = update.ms + actual.query_compute_ms;
    if (state !== "initial") updateQueryTotalMs += updateQueryMs;
    finalChecksum = actual.checksum;
    finalInputHash = inputHash;
    console.log(JSON.stringify({
      event: "mutation",
      status,
      state,
      affected_rows: affectedRows,
      expected_affected_rows: expectedAffected,
      join_affected_rows: state === "dimension_fanout" ? fanout : affectedRows,
      update_transaction_ms: update.ms,
      update_plus_query_ms: updateQueryMs,
      ...actual,
      expected_checksum: expected.checksum,
      input_hash: inputHash,
      exact_input_output_validated: exact,
      memory,
      ...common,
    }));
    if (status !== "ok") throw new Error(`${maintenance} mismatch after ${state}`);
  }
  console.log(JSON.stringify({
    event: "case-total",
    status: "ok",
    update_plus_query_ms: updateQueryTotalMs,
    wall_ms: elapsedMs(wallStarted),
    final_checksum: finalChecksum,
    final_input_hash: finalInputHash,
    disk: await adapter.disk(),
    ...common,
  }));
}
