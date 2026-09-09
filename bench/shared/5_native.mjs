import { execFileSync } from "node:child_process";
import pg from "pg";
import { runCase } from "./3_run_case.mjs";

const { Client } = pg;

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? fallback : process.argv[index + 1];
}

const maintenance = argument("maintenance", "query");
const rowCount = Number(argument("rows", "128"));
const batchSize = Number(argument("batch", "1"));
const profile = argument("profile", "smoke");
if (!new Set(["query", "pg_ivm"]).has(maintenance)) throw new Error(`bad maintenance: ${maintenance}`);

const client = new Client({
  host: process.env.PGHOST,
  port: Number(process.env.PGPORT ?? "5432"),
  database: process.env.PGDATABASE,
  user: process.env.PGUSER,
});

function rssFor(pid) {
  try {
    const output = execFileSync("/bin/ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8" }).trim();
    return output ? Number(output) : null;
  } catch {
    return null;
  }
}

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
    const versions = await client.query(`
      SELECT current_setting('server_version') AS postgres_version,
             (SELECT extversion FROM pg_extension WHERE extname = 'pg_ivm') AS pg_ivm_version
    `);
    const durability = await client.query(`
      SELECT current_setting('fsync') AS fsync,
             current_setting('synchronous_commit') AS synchronous_commit,
             current_setting('full_page_writes') AS full_page_writes
    `);
    return {
      runtime: "native PostgreSQL",
      ...versions.rows[0],
      durability: { transport: "private Unix-domain socket", ...durability.rows[0] },
    };
  },
  async memory() {
    const backendPid = client.processID;
    const backendMemory = await client.query("SELECT sum(total_bytes)::bigint AS bytes FROM pg_backend_memory_contexts");
    const usage = process.memoryUsage();
    return {
      scope: "connected PostgreSQL backend plus Node client; shared pages may appear in backend RSS",
      backend_pid: backendPid,
      backend_rss_kb: rssFor(backendPid),
      backend_memory_context_kb: Math.round(Number(backendMemory.rows[0].bytes) / 1024),
      client_rss_kb: Math.round(usage.rss / 1024),
      client_heap_used_kb: Math.round(usage.heapUsed / 1024),
      client_process_peak_rss_kb: process.resourceUsage().maxRSS,
    };
  },
};

try {
  await client.connect();
  await client.query("DROP SCHEMA public CASCADE; CREATE SCHEMA public");
  await runCase(adapter, {
    engine: "native-postgres",
    maintenance,
    rowCount,
    batchSize,
    profile,
  });
} finally {
  await client.end();
}
