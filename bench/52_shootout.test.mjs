import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {circuitStatus,performanceRows,parseRecords,ddContracts} from './51_shootout_report.mjs';
import {makeCircuitFixture} from './shared/30_circuit_workload.mjs';
function receipt(repetitions=3,rows=24){const fixture=makeCircuitFixture('pipeline',rows,3,4);const context={circuit:'pipeline',maintenance:'dd',rows,batch_size:3,fanout:4,run_kind:'measured'};
 return [{event:'run-metadata',cases:[{circuit:'pipeline',rows,batch_size:3,fanout:4}],arms:['dd'],repetitions},...Array.from({length:repetitions},(_,i)=>[
  ...fixture.states.map((s,j)=>({...context,event:'mutation',repetition:i+1,state:s.name,status:'ok',exact_input_output_validated:true,input_hash:s.input_hash,checksum:s.expected.checksum,update_plus_query_ms:i+j+1})),
  {...context,event:'case-total',repetition:i+1,status:'ok',update_plus_query_ms:fixture.states.reduce((n,_,j)=>n+i+j+1,0)},
  {...context,event:'all-arm-run',repetition:i+1,status:'ok',all_input_output_states_match:true},
 ]).flat(),{event:'run-done',status:'ok'}];
}
const gate=[{suite:'circuits',case:'pipeline',engine:'dd',status:'ok'}];
test('semantic gate rejects missing, duplicate, and invalid state receipts and preserves unsupported/errors',()=>{
 const good=receipt(1);assert.deepEqual(circuitStatus(good,'dd','pipeline'),{status:'ok',states:13,algorithm:undefined});
 const removed=good.filter(r=>r.state!=='cycle_break');assert.equal(circuitStatus(removed,'dd','pipeline').status,'incomplete');
 const duplicate=[...good,good.find(r=>r.event==='mutation')];assert.equal(circuitStatus(duplicate,'dd','pipeline').status,'incomplete');
 const corrupt=structuredClone(good);corrupt.find(r=>r.event==='mutation').exact_input_output_validated=false;assert.equal(circuitStatus(corrupt,'dd','pipeline').status,'incomplete');
 const hashFault=structuredClone(good);hashFault.find(r=>r.event==='mutation').checksum='invented';assert.equal(circuitStatus(hashFault,'dd','pipeline').status,'mismatch');
 const context={circuit:'pipeline',maintenance:'dd',run_kind:'measured'};
 assert.deepEqual(circuitStatus([{...context,event:'capability',status:'unsupported',reason:'engine rejected query'}],'dd','pipeline'),{status:'unsupported',reason:'engine rejected query'});
 assert.deepEqual(circuitStatus([{...context,event:'case-status',status:'timeout',reason:'deadline'}],'dd','pipeline'),{status:'timeout',reason:'deadline'});
 assert.throws(()=>parseRecords('{broken}\n'),/invalid JSON receipt/);
});
test('timing excludes warmups, needs all repetitions and semantic acceptance, and preserves cell dimensions',()=>{
 const records=receipt();records.push({...records.find(r=>r.event==='mutation'),run_kind:'warmup',update_plus_query_ms:99999});
 assert.deepEqual(performanceRows(records,gate),[{case:'pipeline',rows:24,batch:3,fanout:4,engine:'dd',status:'ok',repetitions:3,total_ms:104,initial_ms:2,clear_ms:9,changes_ms:93,algorithm:undefined,durability:undefined}]);
 const missing=records.filter(r=>!(r.event==='mutation'&&r.repetition===2&&r.state==='cycle_break'));assert.equal(performanceRows(missing,gate)[0].status,'excluded');assert.equal(performanceRows(records,[])[0].status,'excluded');
 const duplicate=[...records,records.find(r=>r.event==='case-total')];assert.equal(performanceRows(duplicate,gate)[0].status,'excluded');
 const other=receipt(3,400);const combined=receipt();combined[0].cases.push(other[0].cases[0]);combined.push(...other.slice(1));
 assert.deepEqual(performanceRows(combined,gate).map(r=>[r.rows,r.repetitions,r.status]),[[24,3,'ok'],[400,3,'ok']]);
});
test('DD executable validates every declared contract and detects injected wrong algebra',()=>{
 const bin=path.resolve(import.meta.dirname,'target/release/dd_contracts');
 const good=spawnSync(bin,[],{encoding:'utf8',timeout:60000});assert.equal(good.status,0,good.stderr);const rows=parseRecords(good.stdout);assert.deepEqual(rows.map(r=>r.case),ddContracts);assert(rows.every(r=>r.status==='ok'&&r.states>0));
 const bad=spawnSync(bin,['--inject-fault'],{encoding:'utf8',timeout:60000});assert.equal(bad.signal,null);assert.notEqual(bad.status,0);assert.match(bad.stderr,/signed_map_filter_flatmap step 0/);
});
test('Prolog computes values independently and rejects a wrong expected bag',()=>{
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'ivm-prolog-fault-'));
 try{
  const source={a:[[1,1,7,'apple'],[2,1,null,'pear']],b:[],c:[]};const f={case:{name:'nullable_sum'},states:[{step:0,inputs:source,expected:[[1,2,7]]}]};const file=path.join(dir,'case.json');
  const run=()=>spawnSync('swipl',['-q','-s',path.join(import.meta.dirname,'50_feature_prolog.pl'),'--',dir],{encoding:'utf8',timeout:10000});
  fs.writeFileSync(file,JSON.stringify(f));let result=run();assert.equal(result.status,0,result.stderr);assert.deepEqual(parseRecords(result.stdout).map(r=>[r.engine,r.case,r.status,r.states]),[['prolog','nullable_sum','ok',1]]);
  f.states[0].expected[0][2]=8;fs.writeFileSync(file,JSON.stringify(f));result=run();assert.notEqual(result.status,0);assert.match(result.stderr,/result_mismatch/);
 }finally{fs.rmSync(dir,{recursive:true,force:true});}
});
