import fs from 'node:fs';
import path from 'node:path';
import {circuits,makeCircuitFixture} from './shared/30_circuit_workload.mjs';
import {semanticCircuits,makeSemanticFixture} from './shared/36_semantic_catalog.mjs';
export const engines=['sqlite-ivm','dd','prolog','pg-ivm','pg-query','sqlite-query','pglite-ivm','pglite-query'];
export const arms={'sqlite-ivm':'sqlite-plugin-delta',dd:'dd',prolog:'swi-circuit','pg-ivm':'pg_ivm','pg-query':'query','sqlite-query':'sqlite-query','pglite-ivm':'pglite-ivm','pglite-query':'pglite-query'};
export const ddContracts=['signed_map_filter_flatmap','signed_concat_negate','weighted_join','shared_arrangement_join','custom_weighted_reduce','count_total','positive_threshold','binary_transitive_closure','mutual_even_odd_recursion','recursive_min_distance','nested_fixed_points','stratified_antijoin_after_recursion','incomparable_times_join_lub_and_held_frontier'];
export const circuitNames=[...Object.keys(circuits),...Object.keys(semanticCircuits)];
const features=JSON.parse(fs.readFileSync(new URL('../tests/fixtures/1_features.json',import.meta.url)));
export const featureNames=features.cases.map(c=>c.name);
const aliases={'sqlite-ivm-native':'sqlite-ivm',pg_ivm:'pg-ivm','postgres-query':'pg-query'};
export const median=xs=>{const s=xs.toSorted((a,b)=>a-b);return s.length%2?s[(s.length-1)/2]:(s[s.length/2-1]+s[s.length/2])/2;};
const percentile=(xs,p)=>{const s=xs.toSorted((a,b)=>a-b);return s[Math.max(0,Math.ceil(p*s.length)-1)];};
const distribution=xs=>({samples:xs.length,p50_ms:percentile(xs,.5),p95_ms:percentile(xs,.95),p99_ms:percentile(xs,.99)});
export function parseRecords(text){return text.split('\n').filter(x=>x.trim()).map((line,i)=>{try{return JSON.parse(line);}catch{throw new Error(`invalid JSON receipt at line ${i+1}`);}});}
const fixtureCache=new Map();
function expectedFixture(spec){const key=JSON.stringify([spec.circuit,spec.rows,spec.batch_size,spec.fanout]);if(!fixtureCache.has(key))fixtureCache.set(key,(Object.hasOwn(semanticCircuits,spec.circuit)?makeSemanticFixture:makeCircuitFixture)(spec.circuit,spec.rows,spec.batch_size,spec.fanout));return fixtureCache.get(key);}
export function circuitStatus(records,engine,name,expectedStates=13){
 const selected=records.filter(r=>r.maintenance===arms[engine]&&r.circuit===name&&r.run_kind==='measured');
 const bad=selected.find(r=>['error','mismatch','timeout','resource-blocked','oom-or-external-sigkill'].includes(r.status));
 if(bad)return {status:bad.status,reason:bad.reason??bad.stderr};
 const excluded=selected.find(r=>r.event==='capability');if(excluded)return {status:excluded.status,reason:excluded.reason};
 const mutations=selected.filter(r=>r.event==='mutation');const totals=selected.filter(r=>r.event==='case-total');
 const names=new Set(mutations.map(r=>r.state));
 const metadata=records.find(r=>r.event==='run-metadata');const spec=metadata?.cases.find(c=>c.circuit===name);
 const accepted=records.find(r=>['all-arm-run','circuit-admitted-run'].includes(r.event)&&r.circuit===name&&r.status==='ok'&&r.all_input_output_states_match);
 if(!accepted||mutations.length!==expectedStates||names.size!==expectedStates||totals.length!==1||mutations.some(r=>r.status!=='ok'||!r.exact_input_output_validated||!r.input_hash||!r.checksum)||!spec)
  return {status:'incomplete',reason:'missing or invalid exact state, input/output, total, or cross-engine receipt'};
 const expected=expectedFixture(spec);
 if(mutations.some(r=>{const state=expected.states.find(s=>s.name===r.state);return !state||r.input_hash!==state.input_hash||r.checksum!==state.expected.checksum;}))return {status:'mismatch',reason:'receipt hashes disagree with the independent fixture'};
 return {status:'ok',states:mutations.length,algorithm:selected.find(r=>r.event==='case-setup')?.algorithm};
}
export function performanceRows(records,gate){
 const metadata=records.find(r=>r.event==='run-metadata');if(!metadata)return [];
 const result=[];
 for(const spec of metadata.cases)for(const engine of Object.keys(arms)){
  if(!metadata.arms.includes(arms[engine]))continue;
  const acceptedGate=gate.find(r=>r.suite==='circuits'&&r.case===spec.circuit&&r.engine===engine)?.status==='ok';
  const expected=expectedFixture(spec);
  const selected=records.filter(r=>r.circuit===spec.circuit&&r.maintenance===arms[engine]&&r.rows===spec.rows&&r.batch_size===spec.batch_size&&r.fanout===spec.fanout&&r.run_kind==='measured');
  const totals=selected.filter(r=>r.event==='case-total');const expectedReps=metadata.repetitions;
  const sums=[];
  for(let repetition=1;repetition<=expectedReps;repetition++){
   const rows=selected.filter(r=>r.repetition===repetition&&r.event==='mutation');const total=totals.filter(r=>r.repetition===repetition);
   const exact=records.find(r=>['all-arm-run','circuit-admitted-run'].includes(r.event)&&r.circuit===spec.circuit&&r.rows===spec.rows&&r.batch_size===spec.batch_size&&r.fanout===spec.fanout&&r.repetition===repetition&&r.status==='ok'&&r.all_input_output_states_match);
   if(!exact||rows.length!==13||new Set(rows.map(r=>r.state)).size!==13||total.length!==1||rows.some(r=>r.status!=='ok'||!r.exact_input_output_validated||!Number.isFinite(r.update_plus_query_ms)||!expected.states.some(s=>s.name===r.state&&s.input_hash===r.input_hash&&s.expected.checksum===r.checksum))||!Number.isFinite(total[0].update_plus_query_ms))break;
   sums.push({total:total[0].update_plus_query_ms,initial:rows.find(r=>r.state==='initial').update_plus_query_ms,clear:rows.find(r=>r.state==='clear_edges').update_plus_query_ms,changes:rows.filter(r=>!['initial','clear_edges'].includes(r.state)).reduce((s,r)=>s+r.update_plus_query_ms,0)});
  }
  const complete=acceptedGate&&sums.length===expectedReps&&expectedReps>0;
  const rejected=selected.find(r=>r.event==='capability');
  const state_inventories=complete?selected.filter(r=>r.repetition===1&&r.event==='mutation'&&r.state_inventory).map((r,index)=>({state:r.state,sequence:index,measured_at:r.state_inventory.measured_at,inventory:r.state_inventory})):[];
  const rate=(samples,numerator,elapsed)=>{const rates=samples.map(r=>Number.isFinite(r[elapsed])&&r[elapsed]>0?r[numerator]/(r[elapsed]/1000):null);return rates.some(x=>x===null)?{value:null,unit:'rows/second',unavailable_reason:'one or more elapsed samples are zero or non-finite',partial:true,known_samples:rates.filter(x=>x!==null).length}:{value:median(rates),unit:'rows/second',unavailable_reason:null,partial:false};};
  const telemetry=complete?{latency_by_state:Object.fromEntries(expected.states.map(state=>{const samples=selected.filter(r=>r.event==='mutation'&&r.state===state.name).map(r=>r.update_plus_query_ms);return[state.name,distribution(samples)];})),latency_by_phase:{initial:distribution(sums.map(x=>x.initial)),changes:distribution(sums.map(x=>x.changes)),clear:distribution(sums.map(x=>x.clear))},throughput_by_state:Object.fromEntries(expected.states.map(state=>{const samples=selected.filter(r=>r.event==='mutation'&&r.state===state.name);return[state.name,{logical_input_changes_per_second:rate(samples,'affected_rows','update_transaction_ms'),output_rows_per_second:rate(samples,'output_rows','query_compute_ms'),definition:'fixture logical writes per update transaction second; materialized output rows per query second'}];})),process_resources:selected.filter(r=>r.event==='case-process').map(r=>r.process_resources).filter(Boolean)}:undefined;
  result.push({case:spec.circuit,rows:spec.rows,batch:spec.batch_size,fanout:spec.fanout,engine,status:complete?'ok':rejected?.status??'excluded',repetitions:complete?expectedReps:0,...(complete?{total_ms:median(sums.map(x=>x.total)),initial_ms:median(sums.map(x=>x.initial)),clear_ms:median(sums.map(x=>x.clear)),changes_ms:median(sums.map(x=>x.changes)),state_inventories,telemetry}:{}),algorithm:selected.find(r=>r.event==='case-setup')?.algorithm,durability:selected.find(r=>r.event==='case-setup')?.durability});
 }
 return result;
}
export function assemble(directory){
 const manifest=JSON.parse(fs.readFileSync(path.join(directory,'run.json')));const parseErrors=[];
 const read=file=>{const p=path.join(directory,file);if(!fs.existsSync(p))return [];try{return parseRecords(fs.readFileSync(p,'utf8'));}catch(e){parseErrors.push({file,error:e.message});return [];}};
 const circuit=read('circuits.jsonl');const typed=['features/native.jsonl','features/dd.jsonl','features/pg.jsonl','features/pglite.jsonl','features/prolog.jsonl'].flatMap(read);const dd=read('dd-contracts.jsonl');
 const coverage=[];
 for(const [suite,names]of [['circuits',circuitNames],['typed',featureNames],['dd-contracts',ddContracts]])for(const name of names)for(const engine of manifest.engines){
  let cell;const available=manifest.availability[engine];
  if(!available?.available)cell={status:'unavailable',reason:available?.reason};
  else if(suite==='dd-contracts'&&engine!=='dd')cell={status:'adapter-missing',reason:'DD-specific timestamp/weighted/recursive graph contract; no equivalent adapter in this suite'};
  else if(suite==='circuits')cell=circuitStatus(circuit,engine,name);
  else{
   const source=suite==='dd-contracts'?dd:typed;
   const rows=source.filter(r=>r.case===name&&(aliases[r.engine]??r.engine)===engine);
   if(suite==='typed'&&engine==='sqlite-query'){
    const native=source.filter(r=>r.case===name&&r.engine==='sqlite-ivm-native'&&r.status==='ok'&&r.states===features.mutations.length+1);
    cell=native.length===1?{status:'oracle',states:native[0].states,reason:'Separate SQLite connection used by the native feature adapter'}:{status:'incomplete'};
   }else if(rows.length!==1)cell={status:'incomplete',reason:'Expected exactly one engine/case receipt'};
   else{const row=rows[0];const expectedStates=suite==='typed'?features.mutations.length+1:name==='incomparable_times_join_lub_and_held_frontier'?2:5;
    cell={...row,status:row.status==='ok'&&row.states!==expectedStates?'incomplete':row.status};}
  }
  coverage.push({...cell,suite,case:name,engine});
 }
 const performance=read('performance.jsonl');
 const timing=performanceRows(performance,coverage);
 if(!performance.some(r=>r.event==='run-metadata')||!performance.some(r=>r.event==='run-done'))parseErrors.push({file:'performance.jsonl',error:'missing performance metadata or completion'});
 const summary=Object.fromEntries(manifest.engines.map(engine=>[engine,Object.fromEntries(['circuits','typed','dd-contracts'].map(suite=>{const rows=coverage.filter(r=>r.engine===engine&&r.suite===suite);const counts={};for(const r of rows)counts[r.status]=(counts[r.status]??0)+1;return [suite,counts];}))]));
 const bad=coverage.filter(r=>!['ok','oracle','unsupported','adapter-missing'].includes(r.status));
 const phaseFailures=manifest.phases.filter(p=>p.status!==0&&!(p.status===1&&['features-pg','features-pglite'].includes(p.name)&&coverage.some(r=>r.suite==='typed'&&r.status==='mismatch'&&r.engine===(p.name==='features-pg'?'pg-ivm':'pglite-ivm'))));
 const errors=parseErrors.length+phaseFailures.length+bad.filter(r=>r.status!=='mismatch').length+timing.filter(r=>!['ok','unsupported'].includes(r.status)).length;
 return {schema:1,started_at:manifest.started_at,finished_at:manifest.finished_at,profile:manifest.profile,engines:manifest.engines,summary,coverage,timing,phases:manifest.phases,parse_errors:parseErrors,status:errors?'incomplete':bad.some(r=>r.status==='mismatch')?'mismatch':'ok',receipts:directory,revision:manifest.revision,sources:manifest.sources};
}
export function render(report,{color=false,details=false}={}){
 const paint=(s,c)=>color?`\x1b[${c}m${s}\x1b[0m`:s;const lines=[];
 const title=s=>lines.push('',paint(` ${s} `,'1;37;44'));
 title(`IVM SHOOTOUT  ${report.status.toUpperCase()}  ${report.profile}`);
 lines.push('Fresh execution receipts. Counts are cases, never inferred engine support.');
 title('SEMANTIC COVERAGE');
 const count=c=>Object.entries(c).map(([s,n])=>`${n} ${s==='ok'?'pass':s==='adapter-missing'?'unwired':s}`).join(', ');
 const summaryRows=[['engine','circuits','typed SQL','DD contracts'],...report.engines.map(engine=>{const s=report.summary[engine];return [engine,...[s.circuits,s.typed,s['dd-contracts']].map(count)];})];
 const widths=summaryRows[0].map((_,i)=>Math.max(...summaryRows.map(r=>r[i].length)));
 for(const row of summaryRows)lines.push(row.map((cell,i)=>paint(cell.padEnd(widths[i]),i===0?'36':'0')).join('  '));
 title('DD / DBSP OPERATOR CONTRACTS');
 for(const r of report.coverage.filter(r=>r.suite==='dd-contracts'&&r.engine==='dd'))lines.push(`${paint((r.status==='ok'?'PASS':r.status.toUpperCase()).padEnd(12),r.status==='ok'?'92':'91')} ${r.case}  [${r.states??0} states]`);
 for(const suite of details?['circuits','typed']:['circuits']){
  title(suite==='circuits'?'SHARED CIRCUIT OPERATORS':'TYPED SQL COMPOSITIONS');
  lines.push('case'.padEnd(27)+report.engines.map(e=>e.padEnd(14)).join(''));
  for(const name of suite==='circuits'?circuitNames:featureNames){let line=name.padEnd(27);for(const engine of report.engines){const r=report.coverage.find(r=>r.suite===suite&&r.case===name&&r.engine===engine);const status=r.status==='ok'?'PASS':r.status==='unsupported'?'UNSUPPORTED':r.status==='adapter-missing'?'UNWIRED':r.status.toUpperCase();line+=paint(status.padEnd(14),r.status==='ok'?'92':r.status==='unsupported'?'93':'91');}lines.push(line);}
 }
 title('MISMATCHES / INCOMPLETE EXECUTION');
 const bad=report.coverage.filter(r=>!['ok','oracle','unsupported','adapter-missing'].includes(r.status));
 for(const r of bad)lines.push(`${paint(r.status.toUpperCase(),'91')} ${r.engine} ${r.suite}/${r.case}: ${r.reason??r.restriction??(r.first_mismatch?`state ${r.first_mismatch.step}: ${r.first_mismatch.mutation}`:'missing receipt')}`);
 if(!bad.length)lines.push('No mismatches or missing execution receipts.');
 for(const phase of report.phases.filter(p=>p.status!==0))lines.push(`phase ${phase.name}: exit ${phase.status}; ${phase.log}`);
 title('PERFORMANCE: VALIDATED CASES ONLY');
 lines.push('Times include mutation + maintenance completion + result read. Warmups excluded.');
 lines.push('case                      rows  batch fanout engine          reps  initial ms  changes ms  clear ms');
 for(const r of report.timing.filter(r=>r.status==='ok'))lines.push(`${r.case.padEnd(25)} ${String(r.rows).padStart(5)} ${String(r.batch).padStart(6)} ${String(r.fanout).padStart(6)} ${r.engine.padEnd(15)} ${String(r.repetitions).padStart(4)} ${r.initial_ms.toFixed(3).padStart(11)} ${r.changes_ms.toFixed(3).padStart(11)} ${r.clear_ms.toFixed(3).padStart(9)}`);
 lines.push('changes = median sum of 11 mutation states; initial load and whole-edge deletion separate.');
 title('TELEMETRY: INSTRUMENTED PROCESS / DATABASE RUN');
 lines.push('case                      engine          change p95 ms   initial rows/s   peak RSS MiB   input ops  output ops');
 for(const r of report.timing.filter(r=>r.status==='ok')){const resources=r.telemetry?.process_resources??[];const peak=Math.max(...resources.map(x=>x?.peak_rss_bytes?.value??0));const inputs=resources.map(x=>x?.filesystem_input_operations?.value).filter(Number.isFinite);const outputs=resources.map(x=>x?.filesystem_output_operations?.value).filter(Number.isFinite);const throughput=r.telemetry?.throughput_by_state?.initial?.logical_input_changes_per_second?.value;lines.push(`${r.case.padEnd(25)} ${r.engine.padEnd(15)} ${String(r.telemetry?.latency_by_phase?.changes?.p95_ms?.toFixed(3)??'n/a').padStart(13)} ${String(Number.isFinite(throughput)?throughput.toFixed(1):'n/a').padStart(16)} ${String(peak?(peak/1048576).toFixed(1):'n/a').padStart(14)} ${String(inputs.length?inputs.reduce((a,b)=>a+b,0):'n/a').padStart(11)} ${String(outputs.length?outputs.reduce((a,b)=>a+b,0):'n/a').padStart(11)}`);}
 lines.push('RSS covers the adapter, validation, and telemetry. Block I/O counters cover the whole adapter process and do not specify transferred bytes.');
 lines.push('SQLite/PG are durable; DD/Prolog are volatile. Prolog recomputes bag predicates; reach uses incremental tabling. PGlite uses NodeFS.');
 lines.push(`Full matrices and reasons: ${path.join(report.receipts,'coverage.tsv')}`,`Machine-readable report: ${path.join(report.receipts,'report.json')}`);
 return lines.join('\n')+'\n';
}
export function writeReport(directory,options){const report=assemble(directory);fs.writeFileSync(path.join(directory,'report.json'),JSON.stringify(report,null,2)+'\n');fs.writeFileSync(path.join(directory,'coverage.tsv'),['suite\tcase\tengine\tstatus\tstates\treason',...report.coverage.map(r=>[r.suite,r.case,r.engine,r.status,r.states??0,r.reason??r.restriction??''].map(x=>String(x).replaceAll('\t',' ').replaceAll('\n',' ')).join('\t'))].join('\n')+'\n');fs.writeFileSync(path.join(directory,'report.txt'),render(report,{}));process.stdout.write(render(report,options));return report;}
if(process.argv[1]===import.meta.filename){const report=writeReport(path.resolve(process.argv[2]),{details:process.argv.includes('--details'),color:process.stdout.isTTY&&!process.env.NO_COLOR||process.argv.includes('--color')});process.exitCode=report.status==='ok'?0:report.status==='mismatch'?1:2;}
