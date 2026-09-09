import { readFile } from "node:fs/promises";
import { PGlite } from "@electric-sql/pglite";
import { pg_ivm } from "@electric-sql/pglite-pg_ivm";
import { runCase } from "./3_run_case.mjs";

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? fallback : process.argv[index + 1];
}

const maintenance = argument("maintenance", "query");
const rowCount = Number(argument("rows", "128"));
const batchSize = Number(argument("batch", "1"));
const profile = argument("profile", "smoke");
const dataDir = argument("data-dir", undefined);
if (!dataDir) throw new Error("--data-dir is required");
if (!new Set(["query", "pg_ivm"]).has(maintenance)) throw new Error(`bad maintenance: ${maintenance}`);

const packageManifest = JSON.parse(await readFile(new URL("./package.json", import.meta.url), "utf8"));
const database = maintenance === "pg_ivm"
  ? new PGlite(dataDir, { extensions: { pg_ivm } })
  : new PGlite(dataDir);

const adapter = {
  exec: (sql) => database.exec(sql),
  query: (sql) => database.query(sql),
  async transaction(sql) {
    return database.transaction((transaction) => transaction.exec(sql));
  },
  async metadata() {
    const versions = await database.query(`
      SELECT current_setting('server_version') AS postgres_version,
             (SELECT extversion FROM pg_extension WHERE extname = 'pg_ivm') AS pg_ivm_version
    `);
    const durability = await database.query(`
      SELECT current_setting('fsync') AS fsync,
             current_setting('synchronous_commit') AS synchronous_commit,
             current_setting('full_page_writes') AS full_page_writes
    `);
    return {
      runtime: "PGlite",
      pglite_version: packageManifest.dependencies["@electric-sql/pglite"],
      pglite_pg_ivm_package_version: packageManifest.dependencies["@electric-sql/pglite-pg_ivm"],
      ...versions.rows[0],
      durability: { filesystem: "NodeFS", ...durability.rows[0] },
    };
  },
  async memory() {
    const usage = process.memoryUsage();
    return {
      scope: "Node process containing JavaScript and PostgreSQL WASM",
      rss_kb: Math.round(usage.rss / 1024),
      heap_used_kb: Math.round(usage.heapUsed / 1024),
      external_kb: Math.round(usage.external / 1024),
      array_buffers_kb: Math.round(usage.arrayBuffers / 1024),
      process_peak_rss_kb: process.resourceUsage().maxRSS,
    };
  },
};

try {
  await database.waitReady;
  await runCase(adapter, {
    engine: "pglite",
    maintenance,
    rowCount,
    batchSize,
    profile,
  });
} finally {
  await database.close();
}
