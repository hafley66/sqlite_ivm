import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { crossoverInputHash, makeCrossoverFixture } from "./9_crossover_workload.mjs";

test("shared semantic sequence has deterministic exact inputs and keyed transitions", () => {
  const fixture = makeCrossoverFixture(400, 10, 10, true);
  assert.deepEqual(fixture, makeCrossoverFixture(400, 10, 10, true));
  assert.equal(fixture.states.length, 171);
  const source = { dimension: new Map(), fact: new Map() };
  for (const state of fixture.states) {
    for (const rel of ["dimension", "fact"]) {
      for (const id of state.keyed_writes[rel].deletes) source[rel].delete(id);
      for (const row of state.keyed_writes[rel].puts) source[rel].set(row[0], row);
      assert.deepEqual([...source[rel].values()].sort(([a], [b]) => a - b), state.inputs[rel]);
    }
    assert.equal(crossoverInputHash(state.inputs), state.input_hash);
  }
  assert.equal(fixture.states.at(-1).input_hash, "fd386f195a2f07b75649a761c8d90e94ca6eb31fac9c092402359f8f0778e58a");
  assert.equal(fixture.states.at(-1).expected.checksum, "24993af460d300c615be67af77653c00a1c8b6ac5463ceb2bf8d7b82ac8542b8");
});

test("native DD executes every semantic state and rejects a missing keyed write", async () => {
  const directory = await mkdtemp(join(tmpdir(), "crossover-dd-test-"));
  try {
    const path = join(directory, "fixture.json");
    const fixture = makeCrossoverFixture(400, 10, 10, true);
    const binary = process.env.CROSSOVER_DD_BIN ?? new URL("../../../sprefa-store/target/release/examples/crossover_dd", import.meta.url).pathname;
    await writeFile(path, JSON.stringify(fixture));
    const run = spawnSync(binary, [path], { encoding: "utf8", timeout: 120000 });
    assert.equal(run.status, 0, run.stderr);
    const records = run.stdout.trim().split("\n").map(JSON.parse);
    assert.equal(records.filter((row) => row.event === "mutation" && row.exact_input_output_validated).length, 171);
    assert.equal(records.at(-1).final_input_hash, fixture.states.at(-1).input_hash);
    fixture.states[1].keyed_writes.fact.puts.pop();
    await writeFile(path, JSON.stringify(fixture));
    const bad = spawnSync(binary, [path], { encoding: "utf8", timeout: 120000 });
    assert.notEqual(bad.status, 0);
    assert.match(bad.stderr, /assertion .*failed/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("three-way summarizer excludes failed triples and records exact denominator", async () => {
  const directory = await mkdtemp(join(tmpdir(), "crossover-summary-test-"));
  try {
    const input = join(directory, "input.jsonl");
    const caseData = { budget: "test", rows: 400, batch_size: 10, fanout: 10 };
    const rows = [{ event: "run-metadata", budget: "test", cases: [caseData] },
      ...[1, 2, 3, 4, 5].map((value) => ({ event: "three-way-run", status: "ok", all_input_output_states_match: true,
        pg_ivm_ms: value * 4, sqlite_affected_group_ms: value * 2, dd_ms: value, state_count_per_arm: 5, ...caseData })),
      { event: "three-way-run", status: "mismatch", all_input_output_states_match: false, pg_ivm_ms: 999999, ...caseData }];
    await writeFile(input, rows.map((row) => JSON.stringify(row)).join("\n"));
    const output = join(directory, "three-way.tsv");
    const run = spawnSync(process.execPath, [new URL("14_crossover_summarize.mjs", import.meta.url).pathname, input,
      join(directory, "pairs.tsv"), join(directory, "families.tsv"), output], { encoding: "utf8" });
    assert.equal(run.status, 0, run.stderr);
    const [header, data] = (await readFile(output, "utf8")).trimEnd().split("\n").map((line) => line.split("\t"));
    const result = Object.fromEntries(header.map((column, index) => [column, data[index]]));
    assert.deepEqual([result.successful_triples, result.pg_ivm_median_ms, result.sqlite_affected_group_median_ms, result.dd_median_ms,
      result.pg_ivm_over_sqlite_median_ratio, result.pg_ivm_over_dd_median_ratio], ["5", "12.000000", "6.000000", "3.000000", "2.000000", "4.000000"]);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("loaded plugin consumes shared semantic fixture and rejects missing SQL write", async () => {
  const directory=await mkdtemp(join(tmpdir(),"crossover-plugin-test-"));
  try {
    const fixture=makeCrossoverFixture(400,10,10,true);
    const path=join(directory,"fixture.json");
    const extension=process.env.SQLITE_IVM_EXTENSION;
    assert.ok(extension,"SQLITE_IVM_EXTENSION required for loaded-plugin integration gate");
    await writeFile(path,JSON.stringify(fixture));
    const adapter=new URL("26_sqlite_plugin_adapter.py",import.meta.url).pathname;
    const run=spawnSync("python3",[adapter,"--extension",extension,"--fixture",path,"--db",join(directory,"good.sqlite")],{encoding:"utf8",timeout:120000});
    assert.equal(run.status,0,run.stderr);
    assert.equal(run.stdout.trim().split("\n").map(JSON.parse).filter((r)=>r.event==="mutation" && r.exact_input_output_validated).length,171);
    fixture.states[1].mutation_sql="SELECT 1";
    await writeFile(path,JSON.stringify(fixture));
    const bad=spawnSync("python3",[adapter,"--extension",extension,"--fixture",path,"--db",join(directory,"bad.sqlite")],{encoding:"utf8",timeout:120000});
    assert.notEqual(bad.status,0);
    assert.match(bad.stderr,/AssertionError/);
  } finally {await rm(directory,{recursive:true,force:true});}
});

test("all-arm report excludes a failed trial for every engine", async () => {
  const directory=await mkdtemp(join(tmpdir(),"crossover-all-report-"));
  try {
    const cell={budget:"test",rows:400,batch_size:10,fanout:10};
    const records=[{event:"run-metadata",cases:[cell],budget:"test",arms:["sqlite-plugin-delta","dd"]},
      {event:"all-arm-run",status:"ok",all_input_output_states_match:true,...cell,totals:{"sqlite-plugin-delta":2,dd:1},state_count_per_arm:5},
      {event:"all-arm-run",status:"unmeasured-or-mismatch",all_input_output_states_match:false,...cell,totals:{"sqlite-plugin-delta":999,dd:999}}];
    const input=join(directory,"input.jsonl"),output=join(directory,"all.tsv");
    await writeFile(input,records.map(JSON.stringify).join("\n"));
    const run=spawnSync(process.execPath,[new URL("14_crossover_summarize.mjs",import.meta.url).pathname,input,join(directory,"p.tsv"),join(directory,"f.tsv"),join(directory,"t.tsv"),output],{encoding:"utf8"});
    assert.equal(run.status,0,run.stderr);
    const rows=(await readFile(output,"utf8")).trim().split("\n").slice(1).map((r)=>r.split("\t").slice(4,9));
    assert.deepEqual(rows,[["sqlite-plugin-delta","1","2.000000","2.000000","2.000000"],["dd","1","1.000000","1.000000","1.000000"]]);
  } finally {await rm(directory,{recursive:true,force:true});}
});
