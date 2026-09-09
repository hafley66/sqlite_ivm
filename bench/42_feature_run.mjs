import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {createRequire} from 'node:module';
const [mode,directory,binary]=process.argv.slice(2);
const files=fs.readdirSync(directory).filter(f=>f.endsWith('.json')).sort().map(f=>path.join(directory,f));
if(mode==='dd'){
  for(const file of files){const result=spawnSync(binary,[file],{encoding:'utf8',timeout:60000,maxBuffer:8*1024*1024});if(result.status!==0)throw new Error(`${path.basename(file)}: ${result.error??result.stderr}`);process.stdout.write(result.stdout);}
  const original=JSON.parse(fs.readFileSync(files.find(f=>f.endsWith('nullable_sum.json'))));
  const fault=structuredClone(original);fault.states[1].expected[0][1]+=1;
  const file=path.join(directory,'fault.fixture');fs.writeFileSync(file,JSON.stringify(fault));
  const result=spawnSync(binary,[file],{encoding:'utf8',timeout:60000,maxBuffer:1024*1024});
  assert.equal(result.signal,null);assert.notEqual(result.status,0);assert.match(result.stderr,/nullable_sum state 1/);
  console.log(JSON.stringify({engine:'dd',check:'injected_wrong_count',status:'ok',mismatch_detected:true}));
}else if(mode==='pg'){
  const require=createRequire(new URL('./shared/32_circuit_postgres.mjs',import.meta.url));const {Client}=require('pg');
  const client=new Client({database:'postgres'});await client.connect();
  try{
    await client.query('CREATE EXTENSION IF NOT EXISTS pg_ivm');
    const version=(await client.query("SELECT extversion FROM pg_extension WHERE extname='pg_ivm'")).rows[0].extversion;
    let failedCases=0;
    for(const file of files){
      const fixture=JSON.parse(fs.readFileSync(file));const {name,query,pg_query}=fixture.case;
      await client.query('DROP SCHEMA IF EXISTS feature_case CASCADE;CREATE SCHEMA feature_case;SET search_path=feature_case,public');
      await client.query(fixture.schema);const sql=pg_query??query;
      let supported=true,restriction=null;
      try{await client.query("SELECT pgivm.create_immv('result',$1)",[sql]);}
      catch(error){if(error.code!=='0A000')throw error;supported=false;restriction=error.message;}
      const canonical=(result)=>result.rows.map(r=>r.map((v,i)=>v!==null&&[20,21,23,700,701,1700].includes(result.fields[i].dataTypeID)?Number(v):v)).sort((a,b)=>JSON.stringify(a)<JSON.stringify(b)?-1:JSON.stringify(a)>JSON.stringify(b)?1:0);
      const projection=fixture.columns.map(n=>'"'+n.replaceAll('"','""')+'"').join(',');
      let transaction=false,implicitSavepoint=null,mismatches=0,firstMismatch=null;
      for(const state of fixture.states){
        if(state.mutation){
          if(/^SAVEPOINT /i.test(state.mutation)&&!transaction){await client.query('BEGIN');transaction=true;implicitSavepoint=state.mutation.split(' ')[1];}
          await client.query(state.mutation);
          if(/^BEGIN$/i.test(state.mutation))transaction=true;
          if(/^(COMMIT|ROLLBACK)$/i.test(state.mutation))transaction=false;
          if(implicitSavepoint&&state.mutation===`RELEASE ${implicitSavepoint}`){await client.query('COMMIT');transaction=false;implicitSavepoint=null;}
        }
        const oracle=canonical(await client.query({text:sql,rowMode:'array'}));
        assert.deepEqual(oracle,state.expected,`${name} PG query cross-engine state ${state.step}: ${state.mutation}`);
        if(supported){const actual=canonical(await client.query({text:`SELECT ${projection} FROM result`,rowMode:'array'}));if(JSON.stringify(actual)!==JSON.stringify(oracle)){mismatches++;firstMismatch??={step:state.step,mutation:state.mutation,actual,expected:oracle};}}
        for(const [table,pk]of [['a','id'],['b','bid'],['c','cid']]){const input=canonical(await client.query({text:`SELECT * FROM ${table} ORDER BY ${pk}`,rowMode:'array'}));assert.deepEqual(input,state.inputs[table],`${name} ${table} input state ${state.step}`);}
      }
      console.log(JSON.stringify({engine:'postgres-query',case:name,states:fixture.states.length,status:'ok',input_output_verified:true}));
      console.log(JSON.stringify({engine:'pg_ivm',case:name,states:supported?fixture.states.length:0,status:mismatches?'mismatch':supported?'ok':'unsupported',restriction,mismatches,first_mismatch:firstMismatch,pg_ivm_version:version}));
      if(mismatches)failedCases++;
    }
    if(failedCases)process.exitCode=1;
  }finally{await client.end();}
}else{throw new Error('expected dd or pg mode');}
