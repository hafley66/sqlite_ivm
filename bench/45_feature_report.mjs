import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
const [mode,directory,extension]=process.argv.slice(2);
const root=path.resolve(import.meta.dirname,'..');
const read=(f)=>fs.readFileSync(f,'utf8').trim().split('\n').filter(Boolean).map(JSON.parse);
const fixture=JSON.parse(fs.readFileSync(path.join(root,'tests/fixtures/1_features.json')));
const names=fixture.cases.map(c=>c.name).sort();const states=fixture.mutations.length+1;
const hash=(f)=>crypto.createHash('sha256').update(fs.readFileSync(f)).digest('hex');
if(mode==='pg-baseline'||mode==='pglite-baseline'){
  const embedded=mode==='pglite-baseline';
  const receipt=read(path.join(directory,embedded?'pglite.jsonl':'pg.jsonl'));
  const queries=receipt.filter(r=>r.engine===(embedded?'pglite-query':'postgres-query'));assert.deepEqual(queries.map(r=>r.case).sort(),names);for(const q of queries){assert.equal(q.status,'ok');assert.equal(q.states,states);}
  const expected=JSON.parse(fs.readFileSync(path.join(root,embedded?'bench/44a_pglite_1_13_expected.json':'bench/44_pg_ivm_1_15_expected.json')));
  const ivm=receipt.filter(r=>r.engine===(embedded?'pglite-ivm':'pg_ivm'));assert.deepEqual(ivm.map(r=>r.case).sort(),names);
  for(const row of ivm){assert.equal(row.pg_ivm_version,expected.version);const before=expected.cases.find(c=>c.name===row.case);assert.deepEqual({name:row.case,status:row.status,mismatches:row.mismatches,first_mismatch:row.first_mismatch},before,`pg_ivm behavior changed: ${row.case}`);}
  console.log(JSON.stringify({check:embedded?'pglite_pg_ivm_1_13_recorded_behavior':'pg_ivm_1_15_recorded_behavior',status:'ok',ordinary_query_cases:queries.length,maintained_cases:ivm.filter(r=>r.status==='ok').length,unsupported_cases:ivm.filter(r=>r.status==='unsupported').length,known_mismatch_cases:ivm.filter(r=>r.status==='mismatch').length}));
}else if(mode==='manifest'){
  const native=read(path.join(directory,'native.jsonl')),dd=read(path.join(directory,'dd.jsonl'));
  for(const rows of [native,dd.filter(r=>r.case)]){assert.deepEqual(rows.map(r=>r.case).sort(),names);for(const row of rows){assert.equal(row.status,'ok');assert.equal(row.states,states);}}
  assert(dd.some(r=>r.check==='injected_wrong_count'&&r.status==='ok'));
  const files=['Cargo.toml','Cargo.lock',...fs.readdirSync(path.join(root,'src')).filter(f=>f.endsWith('.rs')).map(f=>'src/'+f),...fs.readdirSync(path.join(root,'tests')).filter(f=>f.endsWith('.rs')).map(f=>'tests/'+f),'tests/support/0_database.rs','tests/fixtures/1_features.json','examples/5_feature_case.rs','bench/Cargo.toml','bench/Cargo.lock','bench/40_feature_graphs.rs','bench/41_feature_dd.rs','bench/42_feature_run.mjs','bench/44_pg_ivm_1_15_expected.json','bench/45_feature_report.mjs','scripts/12_features.sh','scripts/13_feature_pg.sh','scripts/14_native_values.sh','scripts/15_pg_baseline.sh','../.github/workflows/sqlite-ivm.yml'];
  const manifest={extension_sha256:hash(extension),sqlite_version:native[0].sqlite_version,cases:names.length,states_per_case:states,native_checks:native.length*states,dd_checks:names.length*states,source_hashes:Object.fromEntries(files.map(f=>[f,hash(path.join(root,f))])),receipt_hashes:Object.fromEntries(['native.jsonl','dd.jsonl',...(fs.existsSync(path.join(directory,'pg.jsonl'))?['pg.jsonl']:[])].map(f=>[f,hash(path.join(directory,f))]))};
  fs.writeFileSync(path.join(directory,'manifest.json'),JSON.stringify(manifest,null,2)+'\n');console.log(JSON.stringify(manifest));
}else{throw new Error('expected pg-baseline or manifest');}
