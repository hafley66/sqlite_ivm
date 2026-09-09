import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { PGlite } from "@electric-sql/pglite";
import { pg_ivm } from "@electric-sql/pglite-pg_ivm";

const pglitePackage = JSON.parse(
  await readFile(new URL("./node_modules/@electric-sql/pglite/package.json", import.meta.url), "utf8"),
);

const dataDir = await mkdtemp(join(tmpdir(), "sprefa-pglite-probe."));
const database = new PGlite(dataDir, { extensions: { pg_ivm } });

try {
  await database.waitReady;
  await database.exec("CREATE EXTENSION pg_ivm");
  const result = await database.query(`
    SELECT current_setting('server_version') AS postgres_version,
           extversion AS pg_ivm_version
      FROM pg_extension
     WHERE extname = 'pg_ivm'
  `);
  console.log(JSON.stringify({
    event: "pglite-pg_ivm-loaded",
    pglite_version: pglitePackage.version,
    ...result.rows[0],
  }));
} finally {
  await database.close();
  await rm(dataDir, { recursive: true, force: true });
}
