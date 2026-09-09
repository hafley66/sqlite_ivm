import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtempSync,writeFileSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {spawnSync} from 'node:child_process';
import {semanticCircuits,makeSemanticFixture} from './36_semantic_catalog.mjs';

test('nine integer semantic families match SQLite, native DD and SWI transitions',{skip:!process.env.SEMANTIC_DD_BIN},()=>{
 const root=mkdtempSync(join(tmpdir(),'ivm-semantic-gate-'));
 try {for(const family of Object.keys(semanticCircuits)) {
  const path=join(root,family+'.json');writeFileSync(path,JSON.stringify(makeSemanticFixture(family)));
  for(const [command,args] of [
   ['python3',[new URL('31b_circuit_sqlite.py',import.meta.url).pathname,'--fixture',path,'--db',join(root,family+'.sqlite')]],
   [process.env.SEMANTIC_DD_BIN,[path]],
   ['swipl',['-q','-s',new URL('39_semantic_swi.pl',import.meta.url).pathname,'--',path]],
  ]) {
   const child=spawnSync(command,args,{encoding:'utf8',timeout:120000});assert.equal(child.status,0,child.stderr);
   assert.equal(child.stdout.trim().split('\n').map(JSON.parse).filter(r=>r.event==='mutation'&&r.exact_input_output_validated).length,13);
  }
 }}finally{rmSync(root,{recursive:true,force:true});}
});
test('loaded plugin explicitly rejects all nine additional query families atomically',{skip:!process.env.SQLITE_IVM_EXTENSION},()=>{
 const root=mkdtempSync(join(tmpdir(),'ivm-semantic-reject-'));
 try {for(const family of Object.keys(semanticCircuits)) {
  const path=join(root,family+'.json');writeFileSync(path,JSON.stringify(makeSemanticFixture(family)));
  const child=spawnSync('python3',[new URL('31b_circuit_sqlite.py',import.meta.url).pathname,'--fixture',path,'--db',join(root,family+'.sqlite'),'--extension',process.env.SQLITE_IVM_EXTENSION],{encoding:'utf8',timeout:120000});
  assert.equal(child.status,0,child.stderr);const records=child.stdout.trim().split('\n').map(JSON.parse);
  assert.equal(records.length,1);assert.equal(records[0].status,'unsupported');assert.equal(records[0].installation_atomic,true);
 }}finally{rmSync(root,{recursive:true,force:true});}
});

test('native semantic graph receipt detects a removed keyed write',{skip:!process.env.SEMANTIC_DD_BIN},()=>{
 const root=mkdtempSync(join(tmpdir(),'ivm-semantic-fault-'));
 try {
  const fixture=makeSemanticFixture('minmax');fixture.states[0].writes.shift();
  const path=join(root,'fault.json');writeFileSync(path,JSON.stringify(fixture));
  const child=spawnSync(process.env.SEMANTIC_DD_BIN,[path],{encoding:'utf8',timeout:120000});
  assert.notEqual(child.status,0);assert.match(child.stderr,/assertion.*failed/s);
 }finally{rmSync(root,{recursive:true,force:true});}
});
