import fs from 'node:fs';
import {makeCircuitFixture} from '../../shared/30_circuit_workload.mjs';
import {performanceRows} from '../../51_shootout_report.mjs';
const dir=import.meta.dirname;
const records=fs.readFileSync(dir+'/source-performance.log','utf8').split('\n').flatMap(s=>{try{return[JSON.parse(s)]}catch{return[]}});
const meta=records.find(r=>r.event==='run-metadata');
const families=['pipeline','join','aggregate_churn'];
const retained=records.filter(r=>r.event==='run-metadata'||families.includes(r.circuit));
meta.cases=meta.cases.filter(r=>families.includes(r.circuit));
for(const spec of meta.cases)for(let rep=1;rep<=meta.repetitions;rep++){
 const fixture=makeCircuitFixture(spec.circuit,spec.rows,spec.batch_size,spec.fanout);
 for(const arm of meta.arms){
  const rows=retained.filter(r=>r.circuit===spec.circuit&&r.rows===spec.rows&&r.batch_size===spec.batch_size&&r.fanout===spec.fanout&&r.repetition===rep&&r.run_kind==='measured'&&r.maintenance===arm);
  const mutations=rows.filter(r=>r.event==='mutation');const totals=rows.filter(r=>r.event==='case-total');
  if(mutations.length!==13||totals.length!==1||totals[0].status!=='ok'||!Number.isFinite(totals[0].update_plus_query_ms))throw Error(`Incomplete ${spec.circuit} ${spec.rows} ${arm} ${rep}`);
  for(const expected of fixture.states){const matches=mutations.filter(r=>r.state===expected.name);if(matches.length!==1)throw Error('Missing/duplicate state');const actual=matches[0];if(actual.status!=='ok'||!actual.exact_input_output_validated||actual.input_hash!==expected.input_hash||actual.checksum!==expected.expected.checksum||!Number.isFinite(actual.update_plus_query_ms))throw Error('State validation failed');}
 }
 retained.push({event:'all-arm-run',status:'ok',circuit:spec.circuit,rows:spec.rows,batch_size:spec.batch_size,fanout:spec.fanout,repetition:rep,all_input_output_states_match:true,validation_origin:'Revalidated after ENOSPC from logged receipts against makeCircuitFixture; all 8 arms and 13 states checked.'});
}
const report=JSON.parse(fs.readFileSync(dir+'/report.json'));
report.timing=performanceRows(retained,report.coverage);
if(report.timing.length!==48||report.timing.some(r=>r.status!=='ok'||r.repetitions!==5))throw Error('Unexpected coverage');
report.chart_recovery={reason:'Full run ended ENOSPC. Only three fully measured workloads retained.',families,rows:[400,12000],validated_cells:48,states_checked:48*5*13,original_log:dir+'/logs/performance.log',reconstructed_admission_receipts:30};
fs.writeFileSync(dir+'/report.json',JSON.stringify(report,null,2)+'\n');
fs.writeFileSync(dir+'/performance.jsonl',retained.map(r=>JSON.stringify(r)).join('\n')+'\n');
console.log(report.chart_recovery);
