import {
  checksumRows,
  distinctQuery,
  groupsFor,
  makeOracle,
  mutationSql,
  recursiveCases,
  recursiveChecksum,
  recursiveEdges,
  seed,
  summaryQuery,
  validateCase,
} from "./2_workload.mjs";

const states = [
  "initial",
  "insert_batch",
  "delete_batch",
  "update_batch",
  "dimension_fanout",
  "duplicate_delete_one",
  "duplicate_delete_two",
];

function elapsedMs(started) {
  return Number(process.hrtime.bigint() - started) / 1_000_000;
}

async function timed(operation) {
  const started = process.hrtime.bigint();
  const value = await operation();
  return { ms: elapsedMs(started), value };
}

function normalizeSnapshot(summaryResult, distinctResult) {
  const summaryRows = summaryResult.rows.map((row) => [
    String(row.group_id),
    String(row.row_count),
    String(row.weighted_sum),
  ]);
  const distinctRows = distinctResult.rows.map((row) => [String(row.group_id), String(row.amount)]);
  return checksumRows(summaryRows, distinctRows);
}

async function readSnapshot(adapter) {
  await adapter.exec("DROP TABLE IF EXISTS bench_summary_snapshot; DROP TABLE IF EXISTS bench_distinct_snapshot");
  const query = await timed(async () => {
    await adapter.exec(`CREATE TEMP TABLE bench_summary_snapshot AS SELECT group_id, row_count, weighted_sum FROM summary_view`);
    await adapter.exec(`CREATE TEMP TABLE bench_distinct_snapshot AS SELECT group_id, amount FROM distinct_view`);
  });
  const transfer = await timed(async () => {
    const summary = await adapter.query("SELECT group_id, row_count, weighted_sum FROM bench_summary_snapshot ORDER BY group_id");
    const distinct = await adapter.query("SELECT group_id, amount FROM bench_distinct_snapshot ORDER BY group_id, amount");
    return { summary, distinct };
  });
  const checksum = await timed(() => normalizeSnapshot(transfer.value.summary, transfer.value.distinct));
  await adapter.exec("DROP TABLE bench_summary_snapshot; DROP TABLE bench_distinct_snapshot");
  return {
    query_readback_ms: query.ms,
    client_transfer_ms: transfer.ms,
    checksum_ms: checksum.ms,
    ...checksum.value,
  };
}

async function createViews(adapter, maintenance) {
  if (maintenance === "pg_ivm") {
    await adapter.exec("CREATE EXTENSION IF NOT EXISTS pg_ivm");
    await adapter.exec(`SELECT pgivm.create_immv('summary_view', $$${summaryQuery}$$)`);
    await adapter.exec(`SELECT pgivm.create_immv('distinct_view', $$${distinctQuery}$$)`);
  } else {
    await adapter.exec(`CREATE VIEW summary_view AS ${summaryQuery}`);
    await adapter.exec(`CREATE VIEW distinct_view AS ${distinctQuery}`);
  }
}

async function runRecursive(adapter, engine, maintenance, profile, common) {
  for (const recursiveCase of recursiveCases(profile)) {
    if (maintenance === "pg_ivm") {
      console.log(JSON.stringify({
        event: "recursive-case",
        category: "recursive-pg_ivm",
        status: "unsupported",
        reason: "pg_ivm rejects WITH RECURSIVE view definitions",
        engine,
        maintenance,
        family: recursiveCase.family,
        n: recursiveCase.nodeCount,
        ...common,
      }));
      continue;
    }
    await adapter.exec("DROP TABLE IF EXISTS recursive_edge; DROP TABLE IF EXISTS recursive_snapshot");
    await adapter.exec("CREATE TEMP TABLE recursive_edge(source integer NOT NULL, target integer NOT NULL)");
    const edges = recursiveEdges(recursiveCase.family, recursiveCase.nodeCount);
    const values = edges.map(([source, target]) => `(${source},${target})`).join(",");
    const setup = await timed(() => adapter.exec(`INSERT INTO recursive_edge VALUES ${values}; CREATE INDEX ON recursive_edge(source)`));
    const query = await timed(() => adapter.exec(`
      CREATE TEMP TABLE recursive_snapshot AS
      WITH RECURSIVE reachable(source, target) AS (
        SELECT source, target FROM recursive_edge
        UNION
        SELECT reachable.source, recursive_edge.target
          FROM reachable
          JOIN recursive_edge ON recursive_edge.source = reachable.target
      )
      SELECT source, target FROM reachable
    `));
    const transfer = await timed(() => adapter.query("SELECT source, target FROM recursive_snapshot ORDER BY source, target"));
    const checksum = await timed(async () => {
      const canonical = transfer.value.rows.map((row) => `${row.source}\t${row.target}`).join("\n");
      return import("node:crypto").then(({ createHash }) => ({
        checksum: createHash("sha256").update(canonical).digest("hex"),
        derived: transfer.value.rows.length,
        transfer_bytes: Buffer.byteLength(canonical),
      }));
    });
    const actual = checksum.value;
    const expected = recursiveChecksum(recursiveCase.family, recursiveCase.nodeCount);
    console.log(JSON.stringify({
      event: "recursive-case",
      category: "recursive-full-query",
      status: actual.checksum === expected.checksum && actual.derived === expected.derived ? "ok" : "mismatch",
      engine,
      maintenance,
      family: recursiveCase.family,
      n: recursiveCase.nodeCount,
      setup_ms: setup.ms,
      query_readback_ms: query.ms,
      client_transfer_ms: transfer.ms,
      checksum_ms: checksum.ms,
      ...actual,
      expected_checksum: expected.checksum,
      ...common,
    }));
  }
}

export async function runCase(adapter, options) {
  const { engine, maintenance, rowCount, batchSize, profile } = options;
  validateCase(rowCount, batchSize);
  const groupCount = groupsFor(rowCount);
  const oracle = makeOracle(rowCount, batchSize);
  const mutations = mutationSql(rowCount, batchSize, groupCount);
  const common = { rows: rowCount, batch_size: batchSize, seed };

  const schema = await timed(() => adapter.exec(`
    CREATE TABLE dimension(group_id integer PRIMARY KEY, factor integer NOT NULL);
    CREATE TABLE fact(id integer PRIMARY KEY, group_id integer NOT NULL REFERENCES dimension(group_id), amount integer NOT NULL)
  `));
  const load = await timed(() => adapter.exec(`
    INSERT INTO dimension
    SELECT group_id, group_id % 7 + 1 FROM generate_series(0, ${groupCount - 1}) AS group_id;
    INSERT INTO fact VALUES (1, 0, 1000000000), (2, 0, 1000000000);
    INSERT INTO fact
    SELECT id, id % ${groupCount}, (id * 37) % 1000 - 500
      FROM generate_series(3, ${rowCount}) AS id
  `));
  const indexes = await timed(() => adapter.exec(`
    CREATE INDEX fact_group_idx ON fact(group_id);
    CREATE INDEX fact_distinct_idx ON fact(group_id, amount)
  `));
  const viewBuild = await timed(() => createViews(adapter, maintenance));
  const indexRows = await adapter.query(`
    SELECT indexname, indexdef FROM pg_indexes
     WHERE schemaname = 'public' AND tablename IN ('fact', 'summary_view', 'distinct_view')
     ORDER BY indexname
  `);
  const metadata = await adapter.metadata();
  console.log(JSON.stringify({
    event: "case-setup",
    category: "nonrecursive-ivm",
    status: "ok",
    engine,
    maintenance,
    group_count: groupCount,
    schema_ms: schema.ms,
    load_ms: load.ms,
    index_ms: indexes.ms,
    view_build_ms: viewBuild.ms,
    indexes: indexRows.rows,
    ...metadata,
    ...common,
  }));

  const wallStarted = process.hrtime.bigint();
  for (const state of states) {
    const update = state === "initial"
      ? { ms: 0 }
      : await timed(() => adapter.transaction(mutations[state]));
    oracle.apply[state]();
    const expected = oracle.snapshot();
    const actual = await readSnapshot(adapter);
    const memory = await adapter.memory();
    const status = actual.checksum === expected.checksum
      && actual.summary_rows === expected.summary_rows
      && actual.distinct_rows === expected.distinct_rows
      ? "ok"
      : "mismatch";
    console.log(JSON.stringify({
      event: "mutation",
      category: "nonrecursive-ivm",
      status,
      engine,
      maintenance,
      state,
      update_transaction_ms: update.ms,
      ...actual,
      expected_checksum: expected.checksum,
      memory,
      ...common,
    }));
    if (status !== "ok") throw new Error(`${engine}/${maintenance} mismatch after ${state}`);
  }
  console.log(JSON.stringify({
    event: "case-done",
    category: "nonrecursive-ivm",
    status: "ok",
    engine,
    maintenance,
    wall_ms: elapsedMs(wallStarted),
    ...common,
  }));
  await runRecursive(adapter, engine, maintenance, profile, common);
}
