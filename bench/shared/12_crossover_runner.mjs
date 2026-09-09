import { execFileSync, spawn } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile, readdir } from "node:fs/promises";
import { arch, hostname, platform, release, totalmem } from "node:os";
import { dirname, join } from "node:path";
import { semanticCircuits, makeSemanticFixture } from "./36_semantic_catalog.mjs";
import { circuits, makeCircuitFixture } from "./30_circuit_workload.mjs";
import { makeCrossoverFixture } from "./9_crossover_workload.mjs";

const circuitCatalog={...circuits,...semanticCircuits};
// Reuse only the current case's immutable fixture. Oracle generation and JSON
// serialization remain outside the measured child update/materialization interval.
let cachedFixtureKey, cachedFixture;
function circuitFixture(circuit,rows,batch,fanout){
  const key=`${circuit}:${rows}:${batch}:${fanout}`;
  if(key!==cachedFixtureKey){cachedFixture=Object.hasOwn(semanticCircuits,circuit)?makeSemanticFixture(circuit,rows,batch,fanout):makeCircuitFixture(circuit,rows,batch,fanout);cachedFixtureKey=key;}
  return cachedFixture;
}

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? fallback : process.argv[index + 1];
}

function caseKey(testCase) {
  return `${testCase.circuit ?? "aggregate"}:${testCase.rows}:${testCase.batch_size}:${testCase.fanout}`;
}

function buildCases(profile, budget) {
  if (profile === "circuits") {
    const families=argument("circuits",Object.keys(circuitCatalog).join(",")).split(",");
    if(families.some(f=>!Object.hasOwn(circuitCatalog,f))) throw new Error("unknown circuit family");
    const grid=argument("circuit-grid","small");
    if(!["small","12k"].includes(grid))throw new Error("circuit-grid must be small or 12k");
    let cells=grid==="small" ? [{rows:24,batch_size:3,fanout:4}] : buildCases("full",budget).filter(c=>c.rows<=12000);
const selected=argument("circuit-cells","").split(",").filter(Boolean);
if(selected.length){
  if(selected.some(cell=>!cells.some(c=>`${c.rows}:${c.batch_size}:${c.fanout}`===cell)))throw new Error("circuit-cells must select existing grid cells");
  cells=cells.filter(c=>selected.includes(`${c.rows}:${c.batch_size}:${c.fanout}`));
}
    return families.flatMap(circuit=>cells.map(c=>({...c,circuit,stage:circuit})));
  }
  if (profile === "semantic") return [{ stage: "semantic", rows: 400, batch_size: 10, fanout: 10 }];
  if (profile === "smoke") {
    return [
      { stage: "smoke", rows: 400, batch_size: 10, fanout: 10 },
      { stage: "smoke", rows: 400, batch_size: 10, fanout: 200 },
    ];
  }
  if (profile === "diagnostic") {
    return [{ stage: "diagnostic", rows: 160_000, batch_size: 10, fanout: 200 }];
  }
  const cases = [
    { stage: "fanout-anchor", rows: 400, batch_size: 10, fanout: 10 },
    { stage: "fanout-anchor", rows: 12_000, batch_size: 10, fanout: 10 },
    { stage: "batch10-row-sweep", rows: 400, batch_size: 10, fanout: 200 },
    { stage: "batch10-row-sweep", rows: 12_000, batch_size: 10, fanout: 200 },
    { stage: "larger-batch10", rows: 160_000, batch_size: 10, fanout: 200 },
    { stage: "batch-sweep", rows: 12_000, batch_size: 1_000, fanout: 200 },
  ];
  if (budget === "constrained") {
    cases.splice(3, 0,
      { stage: "batch10-row-sweep", rows: 1_200, batch_size: 10, fanout: 200 },
      { stage: "batch10-row-sweep", rows: 4_000, batch_size: 10, fanout: 200 });
    cases.splice(cases.length - 1, 0, { stage: "larger-batch10", rows: 40_000, batch_size: 10, fanout: 200 });
    cases.push(
      { stage: "batch-sweep", rows: 12_000, batch_size: 1, fanout: 200 },
      { stage: "batch-sweep", rows: 12_000, batch_size: 100, fanout: 200 });
  }
  return cases;
}

function descendantsRss(rootPid) {
  if (!rootPid) return null;
  try {
    const rows = execFileSync("/bin/ps", ["-axo", "pid=,ppid=,rss="], { encoding: "utf8" })
      .trim()
      .split("\n")
      .map((line) => line.trim().split(/\s+/).map(Number));
    const descendants = new Set([Number(rootPid)]);
    let changed = true;
    while (changed) {
      changed = false;
      for (const [pid, ppid] of rows) {
        if (descendants.has(ppid) && !descendants.has(pid)) {
          descendants.add(pid);
          changed = true;
        }
      }
    }
    return rows.filter(([pid]) => descendants.has(pid)).reduce((sum, row) => sum + row[2], 0);
  } catch {
    return null;
  }
}

function hostSwap() {
  try {
    return execFileSync("/usr/sbin/sysctl", ["vm.swapusage"], { encoding: "utf8" }).trim();
  } catch {
    return null;
  }
}

function memoryFreePercent() {
  if (platform() !== "darwin") return null;
  try {
    const output = execFileSync("/usr/bin/memory_pressure", ["-Q"], { encoding: "utf8" });
    return Number(output.match(/System-wide memory free percentage: (\d+)%/)[1]);
  } catch { return null; }
}

async function fileSha256(path) {
  return createHash("sha256").update(await readFile(path)).digest("hex");
}

const profile = argument("profile", "smoke");
const budget = argument("budget", "constrained");
const outputPath = argument("output", "out/crossover.jsonl");
const runRoot = process.env.IVM_RUN_ROOT;
if (!runRoot) throw new Error("IVM_RUN_ROOT is required");
if (!new Set(["smoke", "full", "diagnostic", "semantic", "circuits"]).has(profile)) throw new Error(`bad profile: ${profile}`);
const timeoutMs = Math.min(120_000, Number(argument("timeout-ms", "120000")));
const deadlineEpochMs = Number(argument("deadline-epoch-ms", String(Date.now() + 20 * 60_000)));
const warmups = Number(argument("warmups", profile === "full" ? "1" : "0"));
const repetitions = Number(argument("repetitions", profile === "full" ? "3" : "1"));
const maxRows = Number(argument("max-rows", "Infinity"));
const cases = buildCases(profile, budget).filter((testCase) => testCase.rows <= maxRows);
const arms = argument("arms", (profile === "circuits" ? "query,pg_ivm,sqlite-query" : "query,pg_ivm")).split(",");
if (arms.some((arm) => !["query", "pg_ivm", "sqlite-query", "swi-circuit", "sqlite-template-group", "sqlite-plugin-delta", "sqlite-plugin-logged", "dd"].includes(arm))) throw new Error(`bad arms: ${arms}`);
if (profile !== "circuits" && arms.some(a=>["sqlite-query","swi-circuit"].includes(a))) throw new Error("circuit-only arm requires circuits profile");
const ddBinary = argument("dd-bin", new URL("../../../sprefa-store/target/release/examples/crossover_dd", import.meta.url).pathname);
const circuitDdBinary = argument("circuit-dd-bin", "");
if(profile==="circuits" && arms.includes("dd") && !circuitDdBinary) throw new Error("--circuit-dd-bin required");
const semanticDdBinary=argument("semantic-dd-bin","");
if(arms.includes("dd") && cases.some(c=>Object.hasOwn(semanticCircuits,c.circuit)) && !semanticDdBinary)throw new Error("--semantic-dd-bin required");
const sqliteBinary = argument("sqlite-bin", "");
const sqliteExtension = argument("sqlite-extension", "");
if (arms.some((arm) => arm.startsWith("sqlite-plugin")) && !sqliteExtension) throw new Error("--sqlite-extension required");
const sqliteProgram = argument("sqlite-program", "");
if (arms.includes("sqlite-template-group") && !sqliteProgram) throw new Error("--sqlite-program is required for sqlite-template-group");
const records = [];
const startedAt = Date.now();
let failed = false;
let resourceBlocked = false;

function append(record) {
  records.push(record);
  process.stdout.write(`${JSON.stringify(record)}\n`);
}

const labDir = new URL(".", import.meta.url);
const baselineFiles = ["results/smoke.jsonl", "results/scale.jsonl", "results/scale-summary.tsv"];
const baseline = [];
for (const file of baselineFiles) {
  baseline.push({ file, sha256: await fileSha256(new URL(file, labDir)) });
}

append({
  event: "run-metadata",
  status: "ok",
  profile,
  budget,
  command: process.argv.join(" "),
  cases,
  arms,
  warmups,
  repetitions,
  engine_order: "warmups then measured rounds; deterministic left rotation by round within each case",
  timing_contract: "ordinary SQL transaction or keyed DD writes, maintenance completion, result materialization and count; exact input/output validation outside timing",
  timeout_ms: timeoutMs,
  total_benchmark_deadline_epoch_ms: deadlineEpochMs,
  memory_control: {
    total_memory_enforcement: "UNENFORCED",
    total_memory_limit_bytes: null,
    cgroup_swap_limit_bytes: null,
    cgroup_oom_events: null,
    cgroup_page_cache_bytes: null,
    hard_cap_axis: "blocked",
    reason: "This invocation does not configure a memory enforcement container",
    observed_group_metric: "PostgreSQL postmaster tree for SQL server arms; worker process tree for SQLite/DD; unavailable samples remain null",
  },
  tuning: {
    shared_buffers: process.env.IVM_SHARED_BUFFERS,
    work_mem: process.env.IVM_WORK_MEM,
    effective_cache_size: process.env.IVM_EFFECTIVE_CACHE_SIZE,
    maintenance_work_mem: process.env.IVM_MAINTENANCE_WORK_MEM,
    temp_file_limit: process.env.IVM_TEMP_FILE_LIMIT,
  },
  host_swap_start: hostSwap(),
  machine: { hostname: hostname(), platform: platform(), release: release(), arch: arch(), total_memory_bytes: totalmem() },
  server_startup_ms: Number(process.env.IVM_SERVER_STARTUP_MS ?? "0"),
  node_version: process.version,
  runner_sha256: await fileSha256(new URL("12_crossover_runner.mjs",labDir)),
  sqlite_sources: sqliteBinary ? await Promise.all((await readdir(new URL("../../src/",labDir))).filter(f=>f.endsWith(".rs")).sort().map(async file=>({file,sha256:await fileSha256(new URL(`../../src/${file}`,labDir))}))) : null,
  circuit_sources: profile === "circuits" ? await Promise.all(["30_circuit_workload.mjs","31_circuit_sqlite.py","32_circuit_postgres.mjs","34_circuit_dd.rs","35_circuit_swi.pl","36_semantic_catalog.mjs","37_semantic_graphs.rs","31a_circuit_postgres.mjs","31b_circuit_sqlite.py","33a_dd_host.rs","38a_semantic_dd.rs","39_semantic_swi.pl"].map(async file=>({file,sha256:await fileSha256(new URL(file,labDir))}))) : null,
  semantic_dd_sha256: semanticDdBinary ? await fileSha256(semanticDdBinary) : null,
  circuit_dd_sha256: circuitDdBinary ? await fileSha256(circuitDdBinary) : null,
  sqlite_adapter_sha256: sqliteBinary ? await fileSha256(sqliteBinary) : null,
  sqlite_adapter_source_sha256: sqliteBinary ? await fileSha256(new URL("../../examples/4_sqlite_case.rs",labDir)) : null,
  sqlite_extension_sha256: sqliteExtension ? await fileSha256(sqliteExtension) : null,
  workload: profile==="circuits" ? {skew:"a key supports controlled by fanout; right-side key 0 receives duplicate supports",churn:"13 named transitions; batch_key_value_move varies batch_size inputs",cardinality:"recorded per mutation",integer_contract:"generated small values; exact bounded integer output"} : {skew:"fixture hot group plus uniform remainder", churn:"four defined mutation families", cardinality:"recorded per mutation"},
  retained_baseline: baseline,
});

async function runProcess(testCase, maintenance, runKind, repetition) {
  const template = maintenance === "sqlite-template-group";
  const plugin = maintenance.startsWith("sqlite-plugin");
  const sqlite = template || plugin || maintenance === "sqlite-query";
  const dd = maintenance === "dd";
  const swi = maintenance === "swi-circuit";
  const rssField = swi ? "swi_process_observed_peak_rss_kb" : sqlite ? "sqlite_process_observed_peak_rss_kb" : dd ? "dd_process_observed_peak_rss_kb" : "postgres_group_observed_peak_rss_kb";
  const context = {
    arm: sqlite ? maintenance : `native-${maintenance}`,
    maintenance,
    run_kind: runKind,
    repetition,
    profile,
    budget,
    stage: testCase.stage,
    circuit: testCase.circuit,
    rows: testCase.rows,
    batch_size: testCase.batch_size,
    fanout: testCase.fanout,
  };
  const freePercent = memoryFreePercent();
  if (freePercent !== null && freePercent < 15) resourceBlocked = true;
  if (resourceBlocked) {
    append({ event: "case-status", status: "resource-blocked", reason: "host free memory below 15%; remaining benchmark cases stopped", memory_free_percent: freePercent, ...context });
    return false;
  }
  if (Date.now() >= deadlineEpochMs) {
    append({ event: "case-status", status: "skipped", reason: "20-minute total benchmark execution budget exhausted", ...context });
    return false;
  }
  const processStarted = process.hrtime.bigint();
  const childRoot = join(runRoot, `${budget}-${maintenance}-${caseKey(testCase)}-${runKind}-${repetition}`);
  await mkdir(childRoot, { recursive: true });
  const fixture = testCase.circuit ? circuitFixture(testCase.circuit,testCase.rows,testCase.batch_size,testCase.fanout) : makeCrossoverFixture(testCase.rows, testCase.batch_size, testCase.fanout, profile === "semantic");
  let args = [
    "11_crossover_native.mjs",
    "--maintenance", maintenance,
    "--rows", String(testCase.rows),
    "--batch", String(testCase.batch_size),
    "--fanout", String(testCase.fanout),
    "--budget", budget,
    "--diagnostic", profile === "diagnostic" ? "1" : "0",
  ];
  if (testCase.circuit && template) {
    append({event:"capability",status:"adapter-missing",reason:"circuit adapter pending",...context});
    return true;
  }
  if (template) {
    const source = await readFile(sqliteProgram, "utf8");
    const program = JSON.parse(source.match(/pub const PROGRAM_JSON: &str = r(#+)"\n([\s\S]*?)\n"\1;/)[2]);
    for (const state of fixture.states) {
      let sql = state.mutation_sql;
      // Semantic VALUES clauses name columns because emitted SQLite tables
      // also have private row-id columns; the shared base relation has none.
      sql = sql.replace(/INSERT INTO fact VALUES/g, "INSERT INTO fact(id,group_id,amount) VALUES")
        .replace(/INSERT INTO dimension VALUES/g, "INSERT INTO dimension(group_id,factor) VALUES");
      for (const rel of program.relations.filter((rel) => program.arrival_targets.includes(rel.rel))) {
        sql = sql.replace(new RegExp(`\\b${rel.rel}\\b`, "g"), `"${rel.table_name}"`);
      }
      state.mutation_sql = sql;
    }
  }
  const fixturePath = join(childRoot, "fixture.json");
  await writeFile(fixturePath, JSON.stringify(fixture));
  if (template) {
    args = ["19_sqlite_template_adapter.py", "--program", sqliteProgram,
      "--fixture", fixturePath, "--db", join(childRoot, "maintained.sqlite"),
      "--sql-output", join(childRoot, "installed.sql")];
  } else if (plugin) {
    args = ["26_sqlite_plugin_adapter.py", "--extension", sqliteExtension, "--metrics", maintenance === "sqlite-plugin-logged" ? "1" : "0",
      "--fixture",fixturePath,"--db",join(childRoot,"maintained.sqlite"),"--sql-output",join(childRoot,"installed.sql")];
  } else if (dd) args = [fixturePath];
  else args.push("--fixture", fixturePath);
  if (testCase.circuit) args = swi ? ["-q","-s",fixture.columns?"39_semantic_swi.pl":"35_circuit_swi.pl","--",fixturePath] : dd ? [fixturePath] : sqlite
    ? [fixture.columns?"31b_circuit_sqlite.py":"31_circuit_sqlite.py","--fixture",fixturePath,"--db",join(childRoot,"circuit.sqlite"),...(plugin?["--extension",sqliteExtension]:[])]
    : [fixture.columns?"31a_circuit_postgres.mjs":"32_circuit_postgres.mjs",fixturePath,maintenance];
  if (testCase.circuit && sqlite && sqliteBinary) args=["--fixture",fixturePath,"--db",join(childRoot,"circuit.sqlite"),...(plugin?["--extension",sqliteExtension]:[])];
  const child = spawn(swi ? "swipl" : sqlite ? (sqliteBinary || "python3") : dd ? (fixture.columns ? semanticDdBinary : testCase.circuit ? circuitDdBinary : ddBinary) : process.execPath, args, {
    cwd: labDir,
    env: {
      ...process.env,
      SQLITE_IVM_LOG_LIMIT: maintenance === "sqlite-plugin-logged" ? "32" : "0",
      PGDATABASE: maintenance === "query" ? process.env.PGDATABASE_NATIVE_QUERY : process.env.PGDATABASE_NATIVE_IVM,
      NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --max-old-space-size=1024`.trim(),
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  let timedOut = false;
  let observedGroupPeakRssKb = 0;
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const timer = setTimeout(() => {
    timedOut = true;
    child.kill("SIGKILL");
  }, Math.min(timeoutMs, Math.max(1, deadlineEpochMs - Date.now())));
  const memoryTimer = setInterval(() => {
    const rss = descendantsRss(sqlite || dd || swi ? child.pid : process.env.IVM_POSTMASTER_PID);
    if (rss !== null) observedGroupPeakRssKb = Math.max(observedGroupPeakRssKb, rss);
  }, 50);
  const exit = await new Promise((resolve) => {
    child.on("exit", (code, signal) => resolve({ code, signal }));
    child.on("error", (error) => { stderr += error.stack; resolve({ code: -1, signal: null }); });
  });
  clearTimeout(timer);
  clearInterval(memoryTimer);
  await writeFile(join(childRoot, "stdout.log"), stdout);
  await writeFile(join(childRoot, "stderr.log"), stderr);
  const processWallMs = Number(process.hrtime.bigint() - processStarted) / 1_000_000;
  if (timedOut) {
    append({ event: "case-status", status: "timeout", reason: `process exceeded ${timeoutMs} ms or total benchmark deadline`,
      [rssField]: observedGroupPeakRssKb || null, ...context });
    return false;
  }
  if (exit.code !== 0) {
    const status = exit.signal === "SIGKILL" ? "oom-or-external-sigkill" : "error";
    append({
      event: "case-status",
      status,
      reason: status === "oom-or-external-sigkill"
        ? "process received SIGKILL outside the benchmark timeout; cgroup OOM attribution is unavailable"
        : "process exited unsuccessfully",
      exit_code: exit.code,
      signal: exit.signal,
      stderr: stderr.slice(0, 8_000),
      [rssField]: observedGroupPeakRssKb || null,
      ...context,
    });
    return false;
  }
  for (const line of stdout.split("\n").filter(Boolean)) {
    try {
      const parsed = JSON.parse(line);
      append({ ...parsed, ...context });
      if (parsed.status !== "ok" && !(parsed.event === "capability" && parsed.status === "unsupported")) failed = true;
    } catch {
      append({ event: "case-status", status: "error", reason: "non-JSON stdout", line: line.slice(0, 2_000), ...context });
      return false;
    }
  }
  append({
    event: "case-process",
    status: "ok",
    process_wall_ms: processWallMs,
    memory_free_percent_before: freePercent,
    [rssField]: observedGroupPeakRssKb || null,
    ...context,
  });
  return true;
}

for (const testCase of cases) {
  for (const [runKind, count] of [["warmup", warmups], ["measured", repetitions]]) {
    for (let repetition = 1; repetition <= count; repetition += 1) {
      const offset = (repetition - 1) % arms.length;
      for (const maintenance of [...arms.slice(offset), ...arms.slice(0, offset)]) {
        if (!await runProcess(testCase, maintenance, runKind, repetition)) failed = true;
      }
    }
  }
}


// Compare every requested arm against the same independent fixture, including
// plugin logging modes. Missing/failed states never count as parity.
for (const testCase of cases) for (let repetition=1; repetition<=repetitions; repetition++) {
  const matching=records.filter((r)=>r.run_kind==="measured" && r.repetition===repetition && caseKey(r)===caseKey(testCase));
  const expected=(testCase.circuit ? circuitFixture(testCase.circuit,testCase.rows,testCase.batch_size,testCase.fanout) : makeCrossoverFixture(testCase.rows,testCase.batch_size,testCase.fanout,profile==="semantic")).states;
  const excluded=matching.filter(r=>r.event==="capability" && ["unsupported","adapter-missing"].includes(r.status)).map(r=>r.maintenance);
  const admitted=arms.filter(a=>!excluded.includes(a));
  const exact=admitted.length>0 && admitted.every((arm)=>expected.every((state)=> {
    const rows=matching.filter((r)=>r.event==="mutation" && r.maintenance===arm && r.state===state.name);
    return rows.length===1 && rows[0].status==="ok" && rows[0].exact_input_output_validated && rows[0].input_hash===state.input_hash && rows[0].checksum===state.expected.checksum;
  }));
  const totals=Object.fromEntries(admitted.map((arm)=>[arm,matching.find((r)=>r.event==="case-total" && r.maintenance===arm)?.update_plus_query_ms ?? null]));
  const ok=exact && Object.values(totals).every((v)=>v!==null);
  if(!ok) failed=true;
  append({event:excluded.length?"circuit-admitted-run":"all-arm-run",status:ok?"ok":"unmeasured-or-mismatch",...testCase,budget,repetition,arms:admitted,excluded_arms:excluded,totals,
    state_count_per_arm:expected.length,all_input_output_states_match:exact,final_input_hash:expected.at(-1).input_hash,final_checksum:expected.at(-1).expected.checksum});
}

if (profile !== "circuits" && ["pg_ivm", "sqlite-template-group", "dd"].every((arm) => arms.includes(arm))) {
  for (const testCase of cases) for (let repetition = 1; repetition <= repetitions; repetition++) {
    const matching = records.filter((row) => row.run_kind === "measured" && row.repetition === repetition && caseKey(row) === caseKey(testCase));
    const totals = ["pg_ivm", "sqlite-template-group", "dd"].map((arm) => matching.find((row) => row.event === "case-total" && row.maintenance === arm));
    const states = matching.filter((row) => row.event === "mutation" && ["pg_ivm", "sqlite-template-group", "dd"].includes(row.maintenance));
    const expectedStates = makeCrossoverFixture(testCase.rows, testCase.batch_size, testCase.fanout, profile === "semantic").states;
    const exact = expectedStates.every((state) => {
      const rows = states.filter((row) => row.state === state.name);
      return rows.length === 3 && rows.every((row) => row.status === "ok" && row.exact_input_output_validated
        && row.input_hash === state.input_hash && row.checksum === state.expected.checksum);
    });
    const status = totals.every(Boolean) && exact ? "ok" : "unmeasured-or-mismatch";
    if (status !== "ok") failed = true;
    append({ event: "three-way-run", status, ...testCase, budget, profile, repetition,
      state_count_per_arm: expectedStates.length, all_input_output_states_match: exact,
      pg_ivm_ms: totals[0]?.update_plus_query_ms ?? null,
      sqlite_affected_group_ms: totals[1]?.update_plus_query_ms ?? null,
      dd_ms: totals[2]?.update_plus_query_ms ?? null,
      final_input_hash: totals[0]?.final_input_hash ?? null, final_checksum: totals[0]?.final_checksum ?? null });
  }
}

if (profile !== "diagnostic" && arms.includes("query") && arms.includes("pg_ivm")) {
  for (const testCase of cases) {
    for (let repetition = 1; repetition <= repetitions; repetition += 1) {
      const matching = records.filter((record) => record.event === "case-total"
        && record.run_kind === "measured"
        && record.repetition === repetition
        && caseKey(record) === caseKey(testCase));
      const query = matching.find((record) => record.maintenance === "query");
      const ivm = matching.find((record) => record.maintenance === "pg_ivm");
      if (!query || !ivm) {
        append({ event: "paired-run", status: "unmeasured", reason: "one or both arms have no successful case-total record", ...testCase, budget, profile, repetition });
        continue;
      }
      append({
        event: "paired-run",
        status: "ok",
        ...testCase,
        budget,
        profile,
        repetition,
        full_query_update_plus_query_ms: query.update_plus_query_ms,
        ivm_update_plus_query_ms: ivm.update_plus_query_ms,
        full_query_over_ivm_speedup: query.update_plus_query_ms / ivm.update_plus_query_ms,
        query_final_checksum: query.final_checksum,
        ivm_final_checksum: ivm.final_checksum,
        checksum_match: query.final_checksum === ivm.final_checksum,
      });
    }
  }
}

append({
  event: "run-done",
  status: failed ? "error" : "ok",
  profile,
  budget,
  benchmark_wall_ms: Date.now() - startedAt,
  host_swap_end: hostSwap(),
});

await mkdir(dirname(outputPath), { recursive: true });
await writeFile(outputPath, `${records.map((record) => JSON.stringify(record)).join("\n")}\n`);
if (failed) process.exitCode = 1;
