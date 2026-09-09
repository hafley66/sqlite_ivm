import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import pg from "pg";
import { runCrossoverCase } from "./10_crossover_case.mjs";

const { Client } = pg;
const processStarted = process.hrtime.bigint();

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? fallback : process.argv[index + 1];
}

function elapsedMs(started) {
  return Number(process.hrtime.bigint() - started) / 1_000_000;
}

function rssFor(pid) {
  try {
    const output = execFileSync("/bin/ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8" }).trim();
    return output ? Number(output) : null;
  } catch {
    return null;
  }
}

const maintenance = argument("maintenance", "query");
const rowCount = Number(argument("rows", "400"));
const batchSize = Number(argument("batch", "10"));
const fanout = Number(argument("fanout", "10"));
const budget = argument("budget", "constrained");
const diagnostic = argument("diagnostic", "0") === "1";
const fixturePath = argument("fixture", "");
const fixture = fixturePath ? JSON.parse(readFileSync(fixturePath, "utf8")) : undefined;
if (!new Set(["query", "pg_ivm"]).has(maintenance)) throw new Error(`bad maintenance: ${maintenance}`);

const client = new Client({
  host: process.env.PGHOST,
  port: Number(process.env.PGPORT ?? "5432"),
  database: process.env.PGDATABASE,
  user: process.env.PGUSER,
});

const adapter = {
  exec: (sql) => client.query(sql),
  query: (sql) => client.query(sql),
  async transaction(sql) {
    await client.query("BEGIN");
    try {
      const result = await client.query(sql);
      await client.query("COMMIT");
      return result;
    } catch (error) {
      await client.query("ROLLBACK");
      throw error;
    }
  },
  async metadata() {
    const result = await client.query(`
      SELECT current_setting('server_version') AS postgres_version,
             (SELECT extversion FROM pg_extension WHERE extname = 'pg_ivm') AS pg_ivm_version,
             current_setting('fsync') AS fsync,
             current_setting('synchronous_commit') AS synchronous_commit,
             current_setting('full_page_writes') AS full_page_writes
    `);
    return {
      runtime: "native PostgreSQL",
      postgres_version: result.rows[0].postgres_version,
      pg_ivm_version: result.rows[0].pg_ivm_version,
      durability: {
        transport: "private Unix-domain socket",
        fsync: result.rows[0].fsync,
        synchronous_commit: result.rows[0].synchronous_commit,
        full_page_writes: result.rows[0].full_page_writes,
      },
    };
  },
  async settings() {
    const result = await client.query(`
      SELECT name, setting, unit
        FROM pg_settings
       WHERE name IN ('shared_buffers', 'work_mem', 'effective_cache_size', 'maintenance_work_mem', 'temp_file_limit')
       ORDER BY name
    `);
    return Object.fromEntries(result.rows.map((row) => [row.name, { setting: row.setting, unit: row.unit }]));
  },
  async disk() {
    const [result, wal, temporary] = await Promise.all([
      client.query(`
        SELECT pg_database_size(current_database())::bigint AS database_bytes,
               coalesce(sum(pg_total_relation_size(quote_ident(schemaname) || '.' || quote_ident(relname))), 0)::bigint AS public_relation_bytes
          FROM pg_stat_user_tables
      `),
      client.query("SELECT coalesce(sum(size), 0)::bigint AS bytes FROM pg_ls_waldir()"),
      client.query("SELECT temp_files::bigint, temp_bytes::bigint FROM pg_stat_database WHERE datname = current_database()"),
    ]);
    return {
      database_bytes: Number(result.rows[0].database_bytes),
      public_relation_bytes: Number(result.rows[0].public_relation_bytes),
      wal_directory_bytes: Number(wal.rows[0].bytes),
      temp_files: Number(temporary.rows[0].temp_files),
      temp_bytes: Number(temporary.rows[0].temp_bytes),
      scope: "current disposable database plus cluster-wide pg_wal; relation bytes include indexes and TOAST",
    };
  },
  async tempIo() {
    const result = await client.query(`
      SELECT temp_files::bigint, temp_bytes::bigint
        FROM pg_stat_database
       WHERE datname = current_database()
    `);
    return result.rows[0];
  },
  async memory() {
    const backendMemory = await client.query("SELECT sum(total_bytes)::bigint AS bytes FROM pg_backend_memory_contexts");
    const usage = process.memoryUsage();
    return {
      scope: "connected backend RSS and memory contexts plus Node client; PostgreSQL process-group peak is sampled by the parent runner",
      backend_pid: client.processID,
      backend_rss_kb: rssFor(client.processID),
      backend_memory_context_kb: Math.round(Number(backendMemory.rows[0].bytes) / 1024),
      client_rss_kb: Math.round(usage.rss / 1024),
      client_heap_used_kb: Math.round(usage.heapUsed / 1024),
      client_process_peak_rss_kb: process.resourceUsage().maxRSS,
      total_memory_limit_bytes: null,
      total_memory_enforcement: "UNENFORCED",
      cgroup_swap_limit_bytes: null,
      cgroup_oom_events: null,
      cgroup_page_cache_bytes: null,
      cgroup_reason: "no usable existing Linux container or VM runtime",
    };
  },
};

try {
  const connectStarted = process.hrtime.bigint();
  await client.connect();
  console.log(JSON.stringify({
    event: "process-startup",
    status: "ok",
    node_start_to_connect_ms: elapsedMs(processStarted),
    connect_ms: elapsedMs(connectStarted),
    maintenance,
    rows: rowCount,
    batch_size: batchSize,
    fanout,
    budget,
  }));
  await client.query("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
  await runCrossoverCase(adapter, { maintenance, rowCount, batchSize, fanout, budget, diagnostic, fixture });
} finally {
  await client.end();
}
