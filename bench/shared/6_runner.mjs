import { execFileSync, spawn } from "node:child_process";
import { mkdir, writeFile } from "node:fs/promises";
import { hostname, platform, release, arch } from "node:os";
import { dirname, join } from "node:path";

function argument(name, fallback) {
  const index = process.argv.indexOf(`--${name}`);
  return index === -1 ? fallback : process.argv[index + 1];
}

const profile = argument("profile", "smoke");
const outputPath = argument("output", "out/receipts.jsonl");
const runRoot = process.env.IVM_RUN_ROOT;
if (!runRoot) throw new Error("IVM_RUN_ROOT is required");
const timeoutMs = Math.min(120_000, Number(argument("timeout-ms", "120000")));
const rssLimitKb = Number(process.env.IVM_RSS_LIMIT_MB ?? "3072") * 1024;
const warmups = Number(argument("warmups", profile === "smoke" ? "0" : "1"));
const repetitions = Number(argument("repetitions", profile === "smoke" ? "1" : "3"));
const defaultCases = profile === "smoke"
  ? "128:1,128:8"
  : "400:1,400:10,12000:10,12000:100,160000:100,160000:1000";
const cases = argument("cases", defaultCases).split(",").map((entry) => {
  const [rows, batchSize] = entry.split(":").map(Number);
  return { rows, batchSize };
});
const skippedCases = profile === "full"
  ? [
      {
        rows: 500_000,
        batch_size: 1_000,
        source_ladder_case: "10x50000",
        reason: "outside the bounded laptop profile after exercising the first three shared store-rig row scales",
      },
      {
        rows: 1_120_000,
        batch_size: 1_000,
        source_ladder_case: "14x80000",
        reason: "outside the bounded laptop profile after exercising the first three shared store-rig row scales",
      },
    ]
  : [];
const requestedArms = argument(
  "arms",
  "pglite-query,pglite-pg_ivm,native-query,native-pg_ivm",
).split(",");
const nativeReady = Boolean(process.env.PGHOST && process.env.PGDATABASE_NATIVE_QUERY && process.env.PGDATABASE_NATIVE_IVM);
const arms = requestedArms.filter((arm) => !arm.startsWith("native-") || nativeReady);
const records = [];
let failed = false;

function append(record) {
  records.push(record);
  process.stdout.write(`${JSON.stringify(record)}\n`);
}

function spawnRss(pid) {
  const output = execFileSync("/bin/ps", ["-o", "rss=", "-p", String(pid)], { encoding: "utf8" }).trim();
  return output ? Number(output) : 0;
}

for (const arm of requestedArms) {
  if (arm.startsWith("native-") && !nativeReady) {
    append({
      event: "arm-status",
      status: "skipped",
      arm,
      reason: "native PostgreSQL environment is unavailable; run ./1_prepare_native.sh then ./7_run.sh",
    });
  }
}

async function runProcess(arm, testCase, runKind, repetition) {
  const processStarted = process.hrtime.bigint();
  const [engine, maintenance] = arm.split("-");
  const script = engine === "pglite" ? "4_pglite.mjs" : "5_native.mjs";
  const childRoot = join(runRoot, `${arm}-${testCase.rows}-${testCase.batchSize}-${runKind}-${repetition}`);
  await mkdir(childRoot, { recursive: true });
  const childArguments = [
    script,
    "--maintenance", maintenance,
    "--rows", String(testCase.rows),
    "--batch", String(testCase.batchSize),
    "--profile", profile,
  ];
  if (engine === "pglite") childArguments.push("--data-dir", join(childRoot, "data"));
  const environment = {
    ...process.env,
    PGDATABASE: maintenance === "query"
      ? process.env.PGDATABASE_NATIVE_QUERY
      : process.env.PGDATABASE_NATIVE_IVM,
    NODE_OPTIONS: `${process.env.NODE_OPTIONS ?? ""} --max-old-space-size=2048`.trim(),
  };

  const child = spawn(process.execPath, childArguments, {
    cwd: new URL(".", import.meta.url),
    env: environment,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  let timedOut = false;
  let memoryBoundExceeded = false;
  const timer = setTimeout(() => {
    timedOut = true;
    child.kill("SIGKILL");
  }, timeoutMs);
  const memoryTimer = setInterval(() => {
    try {
      const output = spawnRss(child.pid);
      if (output > rssLimitKb) {
        memoryBoundExceeded = true;
        child.kill("SIGKILL");
      }
    } catch {}
  }, 100);
  const exit = await new Promise((resolve) => child.on("exit", (code, signal) => resolve({ code, signal })));
  clearTimeout(timer);
  clearInterval(memoryTimer);
  const context = { arm, run_kind: runKind, repetition, profile };
  if (memoryBoundExceeded) {
    append({
      event: "case-status",
      status: "memory-limit",
      reason: `Node client or PGlite process exceeded ${rssLimitKb} KiB RSS`,
      rows: testCase.rows,
      batch_size: testCase.batchSize,
      ...context,
    });
    return false;
  }
  if (timedOut) {
    append({
      event: "case-status",
      status: "timeout",
      reason: `case exceeded ${timeoutMs} ms`,
      rows: testCase.rows,
      batch_size: testCase.batchSize,
      ...context,
    });
    return false;
  }
  if (exit.code !== 0) {
    append({
      event: "case-status",
      status: "error",
      exit_code: exit.code,
      signal: exit.signal,
      stderr: stderr.slice(0, 4000),
      rows: testCase.rows,
      batch_size: testCase.batchSize,
      ...context,
    });
    return false;
  }
  for (const line of stdout.split("\n").filter(Boolean)) {
    let parsed;
    try {
      parsed = JSON.parse(line);
    } catch {
      append({ event: "case-status", status: "error", reason: "non-JSON stdout", line, ...context });
      return false;
    }
    append({ ...parsed, ...context });
    if (parsed.status === "mismatch" || parsed.status === "error") return false;
  }
  append({
    event: "case-process",
    status: "ok",
    process_wall_ms: Number(process.hrtime.bigint() - processStarted) / 1_000_000,
    rows: testCase.rows,
    batch_size: testCase.batchSize,
    ...context,
  });
  return true;
}

append({
  event: "run-metadata",
  status: "ok",
  profile,
  command: process.argv.join(" "),
  cases,
  skipped_cases: skippedCases,
  arms,
  warmups,
  repetitions,
  timeout_ms: timeoutMs,
  memory_bound: {
    node_old_space_mb: 2048,
    node_or_pglite_rss_mb: rssLimitKb / 1024,
    native_shared_buffers: process.env.IVM_SHARED_BUFFERS ?? "128MB",
    native_work_mem: process.env.IVM_WORK_MEM ?? "16MB",
    native_temp_file_limit: process.env.IVM_TEMP_FILE_LIMIT ?? "2048MB",
  },
  machine: { hostname: hostname(), platform: platform(), release: release(), arch: arch() },
  node_version: process.version,
});

for (const skippedCase of skippedCases) {
  append({
    event: "case-status",
    status: "skipped",
    profile,
    ...skippedCase,
  });
}

for (const testCase of cases) {
  for (const arm of arms) {
    for (let warmup = 1; warmup <= warmups; warmup += 1) {
      if (!await runProcess(arm, testCase, "warmup", warmup)) failed = true;
    }
    for (let repetition = 1; repetition <= repetitions; repetition += 1) {
      if (!await runProcess(arm, testCase, "measured", repetition)) failed = true;
    }
  }
}

const checksumByCase = new Map();
for (const record of records) {
  if (record.status !== "ok" || record.run_kind !== "measured") continue;
  if (record.event !== "mutation" && record.category !== "recursive-full-query") continue;
  const key = record.event === "mutation"
    ? `${record.rows}:${record.batch_size}:mutation:${record.state}`
    : `${record.rows}:${record.batch_size}:recursive:${record.family}:${record.n}`;
  const prior = checksumByCase.get(key);
  if (prior && prior !== record.checksum) {
    append({ event: "cross-arm-correctness", status: "mismatch", key, prior, actual: record.checksum, arm: record.arm });
    failed = true;
  } else {
    checksumByCase.set(key, record.checksum);
  }
}
append({ event: "run-done", status: failed ? "error" : "ok", checked_outputs: checksumByCase.size });

await mkdir(dirname(outputPath), { recursive: true });
await writeFile(outputPath, `${records.map((record) => JSON.stringify(record)).join("\n")}\n`);
if (failed) process.exitCode = 1;
