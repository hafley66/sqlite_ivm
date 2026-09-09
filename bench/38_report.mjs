// Summarize exact shared receipts; missing states or totals are errors.
import {readFileSync} from 'node:fs';
const paths=process.argv.slice(2);
if(!paths.length)throw new Error('receipt paths required');
const median=xs=>{const s=xs.toSorted((a,b)=>a-b);return s.length%2?s[(s.length-1)/2]:(s[s.length/2-1]+s[s.length/2])/2;};
for(const path of paths){
  const records=readFileSync(path,'utf8').trim().split('\n').map(JSON.parse);
  if(records.at(-1).event!=='run-done'||records.at(-1).status!=='ok')throw new Error(`incomplete/failed run ${path}`);
  const metadata=records.find(r=>r.event==='run-metadata');
  const totals=records.filter(r=>r.event==='case-total'&&r.run_kind==='measured');
  const mutations=records.filter(r=>r.event==='mutation'&&r.run_kind==='measured');
  const accepted=records.filter(r=>['all-arm-run','circuit-admitted-run'].includes(r.event));
  if(accepted.some(r=>r.status!=='ok'||!r.all_input_output_states_match))throw new Error('parity failure');
  console.log(`\n${path}\n${accepted.length} accepted family/repetition combinations; ${mutations.length} exact mutation checks`);
  console.log('family\trows\tengine\ttotal median ms\tinitial median ms\tclear_edges median ms\tsmall-write states median ms');
  for(const circuit of [...new Set(totals.map(r=>r.circuit))]){
    for(const arm of metadata.arms){
      const t=totals.filter(r=>r.circuit===circuit&&r.maintenance===arm);
      if(!t.length)continue;
      const state=(name)=>median(mutations.filter(r=>r.circuit===circuit&&r.maintenance===arm&&r.state===name).map(r=>r.update_plus_query_ms));
      const small=t.map(r=>mutations.filter(m=>m.circuit===circuit&&m.maintenance===arm&&m.repetition===r.repetition&&!['initial','clear_edges'].includes(m.state)).reduce((s,r)=>s+r.update_plus_query_ms,0));
      console.log([circuit,t[0].rows,arm,median(t.map(r=>r.update_plus_query_ms)),state('initial'),state('clear_edges'),median(small)].map(x=>typeof x==='number'?x.toFixed(3):x).join('\t'));
    }
  }
}
