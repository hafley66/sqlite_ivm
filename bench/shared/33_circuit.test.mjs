import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync,writeFileSync,rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { circuits,makeCircuitFixture,circuitOracle,sortRows } from './30_circuit_workload.mjs';

test('all circuit SQL baselines validate exact inputs and output bags across 13 transitions',()=>{
  const root=mkdtempSync(join(tmpdir(),'ivm-circuit-test-'));
  try {for(const family of Object.keys(circuits)) {
    const fixture=makeCircuitFixture(family);assert.equal(fixture.states.length,13);
    const path=join(root,family+'.json');writeFileSync(path,JSON.stringify(fixture));
    const child=spawnSync('python3',[new URL('31_circuit_sqlite.py',import.meta.url).pathname,'--fixture',path,'--db',join(root,family+'.sqlite')],{encoding:'utf8',timeout:120000});
    assert.equal(child.status,0,child.stderr);
    const records=child.stdout.trim().split('\n').map(JSON.parse);
    assert.equal(records.filter(r=>r.event==='mutation'&&r.exact_input_output_validated).length,13);
  }}finally{rmSync(root,{recursive:true,force:true});}
});
test('bag fanin doubles qualifying support; distinct removes projected duplicates',()=>{
  const tables={a:[[1,0,2],[2,0,2],[3,1,-2],[4,1,-1]],b:[],c:[]};
  assert.deepEqual(sortRows(circuitOracle('fanout_fanin',tables)),[[0,2],[0,2],[0,2],[0,2],[1,-2]]);
  assert.deepEqual(sortRows(circuitOracle('distinct',tables)),[[0,2],[1,-2],[1,-1]]);
});
test('cycle loses all reachability on root retraction and regains it on restore',()=>{
  const states=makeCircuitFixture('reach_cycle').states;
  assert.deepEqual(states.find(s=>s.name==='diamond_path_retract').expected.rows,[[1],[2],[3],[4]]);
  assert.deepEqual(states.find(s=>s.name==='root_retract').expected.rows,[]);
  assert.deepEqual(states.find(s=>s.name==='root_restore').expected.rows,[[1],[2],[3],[4]]);
});
test('injected wrong expected bag is detected by actual SQL adapter',()=>{
  const root=mkdtempSync(join(tmpdir(),'ivm-circuit-failure-'));
  try {
    const fixture=makeCircuitFixture('fanout_fanin');fixture.states[0].expected.rows.pop();
    const path=join(root,'fixture.json');writeFileSync(path,JSON.stringify(fixture));
    const child=spawnSync('python3',[new URL('31_circuit_sqlite.py',import.meta.url).pathname,'--fixture',path,'--db',join(root,'db.sqlite')],{encoding:'utf8',timeout:120000});
    assert.notEqual(child.status,0);assert.match(child.stderr,/initial: output/);
  }finally{rmSync(root,{recursive:true,force:true});}
});

test('native DD catalog and finite frontier check match every fixture state', {skip:!process.env.CIRCUIT_DD_BIN},()=>{
  const root=mkdtempSync(join(tmpdir(),'ivm-circuit-dd-'));
  try {for(const family of Object.keys(circuits)) {
    const path=join(root,family+'.json');writeFileSync(path,JSON.stringify(makeCircuitFixture(family)));
    const child=spawnSync(process.env.CIRCUIT_DD_BIN,[path],{encoding:'utf8',timeout:120000});
    assert.equal(child.status,0,child.stderr);
    const records=child.stdout.trim().split('\n').map(JSON.parse);
    assert.equal(records.filter(r=>r.event==='mutation'&&r.exact_input_output_validated).length,13);
    assert.equal(records.filter(r=>r.event==='frontier-check'&&r.completion_after_all_inputs_advanced).length,1);
  }}finally{rmSync(root,{recursive:true,force:true});}
});

test('SWI bag predicates and incremental recursive reach validate every circuit state',()=>{
  const root=mkdtempSync(join(tmpdir(),'ivm-circuit-swi-'));
  try {for(const family of Object.keys(circuits)) {
    const path=join(root,family+'.json');writeFileSync(path,JSON.stringify(makeCircuitFixture(family)));
    const child=spawnSync('swipl',['-q','-s',new URL('35_circuit_swi.pl',import.meta.url).pathname,'--',path],{encoding:'utf8',timeout:120000});
    assert.equal(child.status,0,child.stderr);
    const records=child.stdout.trim().split('\n').map(JSON.parse);
    assert.equal(records.filter(r=>r.event==='mutation'&&r.exact_input_output_validated).length,13);
    assert.equal(records[0].algorithm,family==='reach_cycle'?'SWI incremental tabling':'SWI full predicate recomputation');
  }}finally{rmSync(root,{recursive:true,force:true});}
});

test('circuit plugin logging preserves exact results and bounded redacted stderr', {skip:!process.env.SQLITE_IVM_EXTENSION},()=>{
  const root=mkdtempSync(join(tmpdir(),'ivm-circuit-logging-'));
  try {
    const path=join(root,'fixture.json');writeFileSync(path,JSON.stringify(makeCircuitFixture('aggregate_churn')));
    const outputs=[];
    for(const limit of [0,32]) {
      const child=spawnSync('python3',[new URL('31_circuit_sqlite.py',import.meta.url).pathname,'--fixture',path,'--db',join(root,`${limit}.sqlite`),'--extension',process.env.SQLITE_IVM_EXTENSION],{encoding:'utf8',timeout:120000,env:{...process.env,SQLITE_IVM_LOG_LIMIT:String(limit)}});
      assert.equal(child.status,0,child.stderr);
      outputs.push(child.stdout.trim().split('\n').map(JSON.parse).filter(r=>r.event==='mutation').map(r=>[r.input_hash,r.checksum]));
      const logs=child.stderr.trim().split('\n').filter(Boolean).map(JSON.parse);
      if(limit===0)assert.equal(logs.length,0);
      else {
        assert(logs.length<=45);
        const maintenance=logs.filter(r=>r.event==='circuit-maintenance-observed');
        assert.equal(maintenance.length,13);
        assert(maintenance.every(r=>r.row_values==='redacted'&&r.sqlite_extended_error_code===0));
        assert(maintenance.at(-1).cumulative_operations>=maintenance[0].cumulative_operations);
      }
    }
    assert.deepEqual(outputs[0],outputs[1]);
  }finally{rmSync(root,{recursive:true,force:true});}
});
