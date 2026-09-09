// Shared SQL adapter core; preserves 32 transport and timers with explicit fixture columns.
import {openPostgres} from './1a_postgres_client.mjs';
import { readFile } from 'node:fs/promises';
import { performance } from 'node:perf_hooks';
import assert from 'node:assert/strict';
import { sortRows, inputText, outputText, digest } from './30_circuit_workload.mjs';
const fixture=JSON.parse(await readFile(process.argv[2],'utf8'));
const embedded=process.argv[3].startsWith('pglite-');
const ivm=['pg_ivm','pglite-ivm'].includes(process.argv[3]);
const db=await openPostgres(embedded,process.argv[4]);
const emit=record=>console.log(JSON.stringify(record));

try {
  const start=performance.now();
  await db.query('DROP SCHEMA public CASCADE; CREATE SCHEMA public');
  for(const table of ['a','b','c'])await db.query(`CREATE TABLE ${table}(id bigint PRIMARY KEY,k bigint NOT NULL,v bigint NOT NULL);CREATE INDEX ON ${table}(k);CREATE INDEX ON ${table}(v)`);
  let query=fixture.query;
  if(ivm) {
    await db.query('CREATE EXTENSION IF NOT EXISTS pg_ivm');
    await db.query('BEGIN');
    try {await db.query('SELECT pgivm.create_immv($1,$2)',['circuit_view',query]);await db.query('COMMIT');}
    catch(error) {
      await db.query('ROLLBACK');
      if(error.code!=='0A000')throw error;
      assert.equal((await db.query("SELECT to_regclass('circuit_view') AS name")).rows[0].name,null);
      emit({event:'capability',status:'unsupported',reason:error.message,sqlstate:error.code,installation_atomic:true});
      process.exitCode=0;
      await db.end();
      process.exit(0);
    }
    query=`SELECT ${Array.from({length:fixture.columns ?? (fixture.circuit==='aggregate_churn'?3:fixture.circuit==='reach_cycle'?1:2)},(_,i)=>`c${i}`).join(',')} FROM circuit_view`;
  }
  emit({event:'case-setup',status:'ok',setup_ms:performance.now()-start,algorithm:ivm?'pg_ivm':'full-query',durability:embedded?'PGlite NodeFS; fsync behavior is WASM host dependent':'fsync synchronous_commit full_page_writes on',runtime:embedded?'PGlite':'PostgreSQL',version:(await db.query('SELECT version()')).rows[0].version});
  let total=0, input_hash,checksum;
  const rows = async sql=>sortRows((await db.query(sql)).rows.map(r=>Object.values(r).map(x=>{const n=Number(x);assert(Number.isSafeInteger(n));return n;})));
  for(const state of fixture.states){
    let start=performance.now();await db.query(`BEGIN;${state.mutation_sql}COMMIT;`);const update=performance.now()-start;
    start=performance.now();const output=await rows(query);const compute=performance.now()-start;
    const inputs={};for(const t of ['a','b','c'])inputs[t]=await rows(`SELECT id,k,v FROM ${t}`);
    for(const t of ['a','b','c'])assert.deepEqual(inputs[t],sortRows(state.inputs[t]),`${state.name} inputs ${t}`);
    assert.deepEqual(output,state.expected.rows,state.name);assert.deepEqual(output,await rows(fixture.query));
    input_hash=digest(inputText(inputs));checksum=digest(outputText(output));assert.equal(input_hash,state.input_hash);assert.equal(checksum,state.expected.checksum);
    total+=update+compute;
    emit({event:'mutation',status:'ok',state:state.name,exact_input_output_validated:true,input_hash,checksum,affected_rows:state.writes.length,output_rows:output.length,output_bytes:Buffer.byteLength(outputText(output)),update_transaction_ms:update,query_compute_ms:compute,update_plus_query_ms:update+compute});
  }
  emit({event:'case-total',status:'ok',update_plus_query_ms:total,final_input_hash:input_hash,final_checksum:checksum,disk:{database_bytes:(await db.query('SELECT pg_database_size(current_database()) AS bytes')).rows[0].bytes}});
} finally {await db.end();}
