import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';

const engines=[
 ['sqlite-ivm','SQLite IVM','#62dba5',7],['dd','Differential Dataflow','#67cfff',9],
 ['prolog','SWI-Prolog','#f9d86a',5],['pg-ivm','Postgres pg_ivm','#ff9970',11],
 ['sqlite-query','SQLite query','#b7ec7b',6],['pg-query','Postgres query','#f66d76',10],
 ['pglite-ivm','PGlite pg_ivm','#97aaff',13],['pglite-query','PGlite query','#eac6a0',12],
];
const arms={'sqlite-plugin-delta':'sqlite-ivm',dd:'dd','swi-circuit':'prolog',pg_ivm:'pg-ivm',query:'pg-query','sqlite-query':'sqlite-query','pglite-ivm':'pglite-ivm','pglite-query':'pglite-query'};
const timingMetrics=[['initial_ms','Initial load'],['changes_ms','11 changes'],['clear_ms','Whole-table deletion']];
const metricPaths=[
 ['table_count','summary.table_count'],['index_count','summary.index_count'],['native_collection_count','summary.native_collection_count'],
 ['total_rows','summary.total_rows'],['table_bytes','summary.table_bytes'],['index_bytes','summary.index_bytes'],['total_relation_bytes','summary.total_relation_bytes'],
 ['database_file_bytes','storage.database_file_bytes'],['wal_file_bytes','storage.wal_file_bytes'],['database_allocated_bytes','storage.database_allocated_bytes'],['peak_rss_bytes','process_memory.peak_rss_bytes'],
];
const circuitOrder=['pipeline','fanout_fanin','distinct','join','self_join','chain','diamond','semijoin','antijoin','aggregate_churn','reach_cycle','minmax','count_distinct','union_set','except_set','intersect_set','topk','window_rank','subquery','cte'];
const q=s=>JSON.stringify(String(s));
const clean=s=>String(s??'').replaceAll('\t',' ').replaceAll('\n',' ');
const finite=x=>Number.isFinite(x)?x:null;
const get=(object,key)=>key.split('.').reduce((value,part)=>value?.[part],object);
const metricValue=metric=>typeof metric==='number'?finite(metric):finite(metric?.value);
const metricReason=metric=>metricValue(metric)===null?(metric?.unavailable_reason??'unavailable'):'';
const formatNumber=value=>value==null||!Number.isFinite(value)?'unavailable':value>=1e6?`${(value/1e6).toFixed(1)}m`:value>=1e3?`${(value/1e3).toFixed(1)}k`:String(Math.round(value*100)/100);
const formatBytes=value=>value==null||!Number.isFinite(value)?'unavailable':value>=1e9?`${(value/1e9).toFixed(2)} GB`:value>=1e6?`${(value/1e6).toFixed(2)} MB`:value>=1e3?`${(value/1e3).toFixed(1)} kB`:`${Math.round(value)} B`;
const slug=s=>String(s).replaceAll(/[^a-zA-Z0-9_]/g,'_');

export function parseJsonLines(text,file='performance.jsonl'){
 return text.split('\n').filter(line=>line.trim()).map((line,index)=>{try{return JSON.parse(line);}catch{throw Error(`${file}:${index+1}: invalid JSON`);}});
}

function snapshotsFromReport(report){
 const rows=[];
 for(const timing of report.timing??[]){
  const lists=[timing.state_inventories,timing.inventory_snapshots].filter(Array.isArray);
  for(const inventory of lists.flat())rows.push({...timing,event:'mutation',...inventory,state_inventory:inventory.state_inventory??inventory.inventory??inventory});
  if(timing.state_inventory)rows.push({...timing,event:'mutation',state:'reported',state_inventory:timing.state_inventory});
 }
 return rows;
}

export function loadInputs(reportFile){
 const report=JSON.parse(fs.readFileSync(reportFile,'utf8'));
 const performanceFile=path.join(path.dirname(reportFile),'performance.jsonl');
 const performance=fs.existsSync(performanceFile)?parseJsonLines(fs.readFileSync(performanceFile,'utf8'),performanceFile):[];
 const mutations=performance.filter(row=>row.event==='mutation'&&row.run_kind==='measured');
 const rawInventories=mutations.filter(row=>row.state_inventory);const inventories=rawInventories.length?rawInventories:snapshotsFromReport(report);
 return {report,performanceFile:fs.existsSync(performanceFile)?performanceFile:null,mutations,inventories};
}

function rowEngine(row){return arms[row.maintenance]??row.engine??row.arm;}
function cellKey(row){return [row.circuit??row.case,row.rows,row.batch_size??row.batch,row.fanout,rowEngine(row)].join('\0');}
function snapshotMetric(row,key){return metricValue(get(row.state_inventory,key));}
function median(values){const sorted=values.filter(Number.isFinite).toSorted((a,b)=>a-b);if(!sorted.length)return null;const i=Math.floor(sorted.length/2);return sorted.length%2?sorted[i]:(sorted[i-1]+sorted[i])/2;}
function quantile(values,p){const sorted=values.filter(Number.isFinite).toSorted((a,b)=>a-b);if(!sorted.length)return null;return sorted[Math.max(0,Math.ceil(p*sorted.length)-1)];}

export function summarizeTelemetry(rows){
 const calculate=selected=>{const latency=selected.map(row=>finite(row.update_plus_query_ms)).filter(value=>value!==null);const throughput=selected.map(row=>row.update_plus_query_ms>0&&Number.isFinite(row.output_rows)?row.output_rows*1000/row.update_plus_query_ms:null).filter(value=>value!==null);return {sample_count:latency.length,latency_ms:{p50:quantile(latency,.5),p95:quantile(latency,.95),p99:quantile(latency,.99)},logical_output_rows_per_second:{p50:quantile(throughput,.5),p95:quantile(throughput,.95),p99:quantile(throughput,.99)},throughput_unavailable_reason:throughput.length?'':selected.some(row=>row.update_plus_query_ms===0)?'elapsed time is zero':'output rows or positive elapsed time unavailable'};};
 const repetitions=[...new Set(rows.map(row=>row.repetition))];const changes=repetitions.map(repetition=>{const selected=rows.filter(row=>row.repetition===repetition&&!['initial','clear_edges'].includes(row.state));return {update_plus_query_ms:selected.reduce((sum,row)=>sum+(finite(row.update_plus_query_ms)??0),0)};});
 const phases={initial:calculate(rows.filter(row=>row.state==='initial')),changes:calculate(changes),clear:calculate(rows.filter(row=>row.state==='clear_edges'))};
 const per_state=Object.fromEntries([...new Set(rows.map(row=>row.state))].map(state=>[state,calculate(rows.filter(row=>row.state===state))]));return {...calculate(rows),phases,per_state};
}

export function summarizeSnapshots(rows){
 const states=[];
 for(const state of [...new Set(rows.map(row=>row.state))]){
  const selected=rows.filter(row=>row.state===state);const metrics={};
  const partial={};for(const [name,key] of metricPaths){metrics[name]=median(selected.map(row=>snapshotMetric(row,key)));partial[name]=selected.some(row=>get(row.state_inventory,key)?.partial===true);}
  metrics.output_rows=median(selected.map(row=>finite(row.output_rows)));
  metrics.output_bytes=median(selected.map(row=>finite(row.output_bytes)));
  states.push({state,metrics,partial});
 }
 const initial=states.find(row=>row.state==='initial')??states[0]??null;
 const final=states.at(-1)??null;const peaks={};
 for(const [name] of [...metricPaths,['output_rows'],['output_bytes']]){
  const available=states.filter(row=>row.metrics[name]!==null).toSorted((a,b)=>b.metrics[name]-a.metrics[name]);
  peaks[name]=available.length?{value:available[0].metrics[name],state:available[0].state,partial:available[0].partial[name]??false}:null;
 }
 return {initial,final,peaks,states};
}

function inventoryRows(rows){
 const result=[];
 for(const row of rows)for(const relation of row.state_inventory?.relations??[]){
  const base={case:row.circuit??row.case,rows:row.rows,batch:row.batch_size??row.batch,fanout:row.fanout,engine:rowEngine(row),repetition:row.repetition,state:row.state,measured_at:row.state_inventory.measured_at,scope:row.state_inventory.scope,relation_name:relation.name,relation_kind:relation.kind,relation_role:relation.role,counted_in_totals:relation.counted_in_totals};
  for(const [name,metric] of [['row_count',relation.row_count],['allocated_bytes',relation.bytes?.allocated],['data_bytes',relation.bytes?.data],['index_bytes',relation.bytes?.index]])result.push({...base,metric:name,value:metricValue(metric),unit:metric?.unit??(name==='row_count'?'rows':'bytes'),unavailable_reason:metricReason(metric)});
 }
 return result;
}

function flattenMetrics(value,prefix='',result=[]){
 if(value==null||['string','number','boolean'].includes(typeof value)){result.push({metric:prefix,value:value??null,unit:'',unavailable_reason:''});return result;}
 if(!Array.isArray(value)&&Object.hasOwn(value,'value')&&(Object.hasOwn(value,'unit')||Object.hasOwn(value,'unavailable_reason'))){result.push({metric:prefix,value:value.value??null,unit:value.unit??'',unavailable_reason:value.unavailable_reason??''});return result;}
 if(Array.isArray(value))value.forEach((item,index)=>flattenMetrics(item,`${prefix}[${index}]`,result));else for(const [key,item] of Object.entries(value))flattenMetrics(item,prefix?`${prefix}.${key}`:key,result);
 return result;
}

function writeData(inputs,out){
 const snapshots=inputs.inventories.map(row=>({case:row.circuit??row.case,rows:row.rows,batch:row.batch_size??row.batch,fanout:row.fanout,engine:rowEngine(row),repetition:row.repetition,state:row.state,output_rows:row.output_rows??null,output_bytes:row.output_bytes??null,state_inventory:row.state_inventory}));
 fs.writeFileSync(path.join(out,'inventory.json'),JSON.stringify({schema:1,source_report:path.resolve(inputs.reportFile??''),snapshots},null,2)+'\n');
 const fields=['case','rows','batch','fanout','engine','repetition','state','measured_at','scope','relation_name','relation_kind','relation_role','counted_in_totals','metric','value','unit','unavailable_reason'];
 fs.writeFileSync(path.join(out,'inventory.tsv'),[fields.join('\t'),...inventoryRows(inputs.inventories).map(row=>fields.map(field=>clean(row[field]??'')).join('\t'))].join('\n')+'\n');
 const groups=new Map();for(const row of inputs.mutations){const key=cellKey(row);if(!groups.has(key))groups.set(key,[]);groups.get(key).push(row);}
 const derived=[...groups.values()].map(rows=>({case:rows[0].circuit,rows:rows[0].rows,batch:rows[0].batch_size,fanout:rows[0].fanout,engine:rowEngine(rows[0]),...summarizeTelemetry(rows)}));
 const report_telemetry=(inputs.report.timing??[]).filter(row=>row.telemetry).map(row=>({case:row.case,rows:row.rows,batch:row.batch,fanout:row.fanout,engine:row.engine,telemetry:row.telemetry}));
 fs.writeFileSync(path.join(out,'telemetry.json'),JSON.stringify({schema:1,derived,report_telemetry,raw_mutation_receipts:inputs.mutations},null,2)+'\n');
 const telemetry=[];for(const row of inputs.mutations){const identity={case:row.circuit,rows:row.rows,batch:row.batch_size,fanout:row.fanout,engine:rowEngine(row),repetition:row.repetition,state:row.state};const copy={...row};delete copy.state_inventory;for(const metric of flattenMetrics(copy))telemetry.push({...identity,source:'receipt',...metric});}
 for(const row of derived)for(const metric of flattenMetrics({sample_count:row.sample_count,latency_ms:row.latency_ms,logical_output_rows_per_second:row.logical_output_rows_per_second}))telemetry.push({...row,repetition:'',state:'all',source:'derived',...metric});
 for(const row of report_telemetry)for(const metric of flattenMetrics(row.telemetry))telemetry.push({...row,repetition:'',state:'all',source:'report',...metric});
 const telemetryFields=['case','rows','batch','fanout','engine','repetition','state','source','metric','value','unit','unavailable_reason'];fs.writeFileSync(path.join(out,'telemetry.tsv'),[telemetryFields.join('\t'),...telemetry.map(row=>telemetryFields.map(field=>clean(row[field]??'')).join('\t'))].join('\n')+'\n');
}

function timingRange(rows){const values=rows.flatMap(row=>timingMetrics.map(([key])=>finite(row[key]))).filter(value=>value>0);if(!values.length)return [0.01,100];return [10**Math.floor(Math.log10(Math.min(...values))),10**Math.ceil(Math.log10(Math.max(...values)))];}
function triplet(summary,name,bytes=false){const peak=summary.peaks[name],format=bytes?formatBytes:formatNumber,mark=value=>value?'*':'';return `${format(summary.initial?.metrics[name])}${mark(summary.initial?.partial[name])}/${format(peak?.value)}${mark(peak?.partial)}/${format(summary.final?.metrics[name])}${mark(summary.final?.partial[name])}`;}
function outputLine(summary){return `logical output rows  initial ${formatNumber(summary.initial?.metrics.output_rows)}   peak ${formatNumber(summary.peaks.output_rows?.value)} @ ${summary.peaks.output_rows?.state??'unavailable'}   final ${formatNumber(summary.final?.metrics.output_rows)}`;}
function processResourceLine(resources=[]){const read=name=>median(resources.map(row=>metricValue(row[name])));return `RSS ${formatBytes(read('peak_rss_bytes'))}  IO ops in/out ${formatNumber(read('filesystem_input_operations'))}/${formatNumber(read('filesystem_output_operations'))}  IO ${formatBytes(read('filesystem_io_bytes'))}`;}

function renderCase(file,caseName,index,timingRows,snapshotRows){
 const [xmin,xmax]=timingRange(timingRows);const lines=[
  'set terminal pngcairo size 2000,1400 noenhanced font "Helvetica,17" background rgb "#101820"',`set output ${q(file)}`,'set multiplot',
  `set label 100 ${q(`IVM SHOOTOUT  /  ${caseName.toUpperCase()}`)} at screen .035,.966 font ',28' tc rgb '#eef4f8' front`,
  `set label 101 ${q('x = measured time (ms, log scale)  /  y = initial rows in source a')} at screen .035,.935 tc rgb '#b3c4d2' front`,
 ];
 const tiers=[...new Set(timingRows.filter(row=>row.status==='ok').map(row=>row.rows))].toSorted((a,b)=>a-b),ymin=Math.max(.1,tiers[0]*.7),ymax=tiers.at(-1)*1.4,ytics=tiers.map(value=>`${q(value.toLocaleString('en-US'))} ${value}`).join(',');
 engines.forEach(([id,name,color,marker],i)=>{const x=.035+(i%4)*.242,y=.902-Math.floor(i/4)*.027;lines.push(`set label ${110+i} ${q(name)} at screen ${x+.016},${y} tc rgb '${color}' font ',15' front`,`set label ${130+i} '' at screen ${x},${y} point pt ${marker} ps 1.25 lc rgb '${color}' front`);});
 for(let panel=0;panel<timingMetrics.length;panel++){
  const [key,title]=timingMetrics[panel],left=.065+panel*.315,right=left+.265;
  lines.push(`set lmargin at screen ${left}`,`set rmargin at screen ${right}`,'set tmargin at screen .80','set bmargin at screen .50',`set title ${q(title)} tc rgb '#eef4f8' font ',20'`,'set logscale x',tiers.length>1?'set logscale y':'unset logscale y',`set xrange [${xmin}:${xmax}]`,`set yrange [${ymin}:${ymax}]`,'set xtics nomirror tc rgb "#c6d3dc"',`set ytics (${ytics}) nomirror tc rgb "#c6d3dc"`,'set tics scale 0','set border 1 lc rgb "#506779"','unset grid','unset key','set xlabel "time (ms)" tc rgb "#b3c4d2"','set ylabel "initial rows" tc rgb "#b3c4d2"');
  tiers.forEach((tier,tierIndex)=>lines.push(`set object ${30+tierIndex} rect from graph 0,first ${tier*.94} to graph 1,first ${tier*1.06} behind fc rgb '#1d2b38' fs solid 1 noborder`));
  const plots=[];for(const [engine,,color,marker] of engines){const selected=timingRows.filter(row=>row.engine===engine&&row.status==='ok'&&row[key]>0).toSorted((a,b)=>a.rows-b.rows);if(!selected.length)continue;const name=`$t_${panel}_${slug(engine)}`;lines.push(`${name} << EOD`,...selected.map(row=>`${row[key]}\t${row.rows}`),'EOD');plots.push(`${name} using 1:2 with linespoints lw 2 pt ${marker} ps 1.45 lc rgb '${color}'`);}
  lines.push(plots.length?`plot ${plots.join(', ')}`:'plot NaN notitle');tiers.forEach((_,tierIndex)=>lines.push(`unset object ${30+tierIndex}`));if(panel===0)lines.push(...[100,101,...engines.flatMap((_,i)=>[110+i,130+i])].map(id=>`unset label ${id}`));
 }
 lines.push('unset logscale','unset border','unset xtics','unset ytics','unset xlabel','unset ylabel','unset title');
 const selectedCells=new Map();const byEngine=new Map();for(const [id] of engines){const cell=timingRows.filter(row=>row.engine===id&&row.status==='ok').toSorted((a,b)=>b.rows-a.rows)[0];if(!cell)continue;const selected=snapshotRows.filter(row=>rowEngine(row)===id&&row.rows===cell.rows&&(row.batch_size??row.batch)===cell.batch&&row.fanout===cell.fanout);selectedCells.set(id,{cell,rows:selected});if(selected.some(row=>row.state_inventory))byEngine.set(id,summarizeSnapshots(selected));}
 lines.push(`set object 20 rect from screen .035,.055 to screen .965,.440 behind fc rgb '#16232e' fs solid 1 border lc rgb '#405667'`,
  `set label 200 'MAINTAINED STATE  /  snapshots sampled after output validation, outside timed region' at screen .055,.413 tc rgb '#eef4f8' font ',19' front`,
  `set label 201 'engine' at screen .055,.382 tc rgb '#92a7b8' font ',13' front`,`set label 202 'counts I/P/F  (T table, I index, N native)' at screen .205,.382 tc rgb '#92a7b8' font ',12' front`,`set label 203 'rows I/P/F' at screen .530,.382 tc rgb '#92a7b8' font ',12' front`,`set label 204 'relation bytes I/P/F' at screen .725,.382 tc rgb '#92a7b8' font ',12' front`);
 engines.forEach(([id,name,color],i)=>{const y=.352-i*.036,summary=byEngine.get(id),selected=selectedCells.get(id);let counts='sensors unavailable',rows='unavailable',bytes='unavailable',storage='';if(summary){counts=`T ${triplet(summary,'table_count')}  I ${triplet(summary,'index_count')}  N ${triplet(summary,'native_collection_count')}`;rows=triplet(summary,'total_rows');bytes=triplet(summary,'total_relation_bytes',true);storage=`DB ${triplet(summary,'database_file_bytes',true)}  WAL ${triplet(summary,'wal_file_bytes',true)}`;}const logicalRows=selected?.rows??[],logical=summarizeSnapshots(logicalRows),derived=summarizeTelemetry(logicalRows),reported=selected?.cell.telemetry?.latency_by_phase;const phase=reported?.changes;const latency=phase?`${formatNumber(phase.p50_ms)}/${formatNumber(phase.p95_ms)}/${formatNumber(phase.p99_ms)} ms n=${phase.samples}`:`${formatNumber(derived.phases.changes.latency_ms.p50)}/${formatNumber(derived.phases.changes.latency_ms.p95)}/${formatNumber(derived.phases.changes.latency_ms.p99)} ms n=${derived.phases.changes.sample_count}`;const resources=selected?.cell.telemetry?.process_resources??[];let extra=derived.sample_count?`@${selected.cell.rows}: change-sum p50/p95/p99 ${latency}  rows/s p50 ${formatNumber(derived.logical_output_rows_per_second.p50)}  ${processResourceLine(resources)}`:'';const status=timingRows.find(row=>row.engine===id)?.status;if(!selected&&status==='unsupported'){counts='unsupported by workload adapter';rows='';bytes='';extra='';}
  lines.push(`set label ${210+i*5} ${q(name)} at screen .055,${y} tc rgb '${color}' font ',12' front`,`set label ${211+i*5} ${q(counts)} at screen .205,${y} tc rgb '#c6d3dc' font ',12' front`,`set label ${212+i*5} ${q(rows)} at screen .530,${y} tc rgb '#c6d3dc' font ',12' front`,`set label ${213+i*5} ${q(bytes)} at screen .725,${y} tc rgb '#c6d3dc' font ',12' front`,`set label ${214+i*5} ${q((storage?storage+'   |   ':'')+extra)} at screen .205,${y-.015} tc rgb '#92a7b8' font ',11' front`);});
 lines.push(`set label 251 ${q(`Detailed relation metrics, nulls, units, limitations, storage, WAL, and RSS: inventory.json + inventory.tsv  |  chart ${String(index+1).padStart(2,'0')}`)} at screen .055,.025 tc rgb '#92a7b8' font ',13' front`,'set origin 0,0','set size 1,1','set xrange [0:1]','set yrange [0:1]','plot 2 notitle lc rgb "#101820"','unset multiplot');
 const run=spawnSync('gnuplot',[],{input:lines.join('\n')+'\n',encoding:'utf8'});if(run.status!==0)throw Error(run.stderr||`gnuplot failed for ${caseName}`);
}

function renderOverview(file,cases,report){
 const ok=report.timing?.filter(row=>row.status==='ok')??[];const counts=Object.fromEntries(engines.map(([id])=>[id,new Set(ok.filter(row=>row.engine===id).map(row=>row.case)).size]));
 const lines=['set terminal pngcairo size 2000,1000 noenhanced font "Helvetica,18" background rgb "#101820"',`set output ${q(file)}`,'set title "IVM SHOOTOUT  /  VERIFIED WORKLOAD COVERAGE" tc rgb "#eef4f8" font ",28" offset 0,-1','set style fill solid .9 border -1','set boxwidth .65 absolute','set xrange [-.75:7.75]','set yrange [0:22]','set border 1 lc rgb "#506779"','unset grid','set ylabel "workload families with validated timing" tc rgb "#b3c4d2"','set xtics rotate by -25 tc rgb "#c6d3dc"','set ytics 2 tc rgb "#c6d3dc"','unset key','$d << EOD',...engines.map(([id,name,color],i)=>`${i}\t${counts[id]}\t${q(name)}\t${q(color)}`),'EOD'];
 const plots=engines.map(([,,color],i)=>`$d using ($1==${i}?$1:1/0):2:xtic(3) with boxes lc rgb '${color}'`).join(', ');lines.push(`plot ${plots}`);
 const run=spawnSync('gnuplot',[],{input:lines.join('\n')+'\n',encoding:'utf8'});if(run.status!==0)throw Error(run.stderr||'gnuplot overview failed');
}

function writeIndexes(out,manifest){
 const links=manifest.charts.map(chart=>chart.file?`- [${chart.case}](${chart.file})`:`- ${chart.case}: ${chart.unavailable_reason}`).join('\n');fs.writeFileSync(path.join(out,'index.md'),`# IVM shootout charts\n\n${manifest.plotting.available?'[Overview](overview.png)':`PNG unavailable: ${manifest.plotting.reason}`} · [Inventory JSON](inventory.json) · [Inventory TSV](inventory.tsv) · [Telemetry JSON](telemetry.json) · [Telemetry TSV](telemetry.tsv) · [Manifest](manifest.json)\n\n${links}\n`);
 const cards=manifest.charts.map(chart=>chart.file?`<li><a href="${chart.file}"><img src="${chart.file}" alt="${clean(chart.case)} benchmark chart"><span>${clean(chart.case)}</span></a></li>`:`<li>${clean(chart.case)}: ${clean(chart.unavailable_reason)}</li>`).join('\n');
 const overview=manifest.plotting.available?'<a href="overview.png">Overview PNG</a>':`PNG unavailable: ${clean(manifest.plotting.reason)}`;fs.writeFileSync(path.join(out,'index.html'),`<!doctype html><meta charset="utf-8"><title>IVM shootout charts</title><style>body{margin:2rem;background:#101820;color:#eef4f8;font:16px system-ui}a{color:#67cfff}nav{margin-bottom:2rem}ul{display:grid;grid-template-columns:repeat(auto-fit,minmax(320px,1fr));gap:1rem;padding:0;list-style:none}li{background:#16232e;border:1px solid #405667;padding:.75rem}img{width:100%;display:block;margin-bottom:.5rem}</style><h1>IVM shootout charts</h1><nav>${overview} · <a href="inventory.json">Inventory JSON</a> · <a href="inventory.tsv">Inventory TSV</a> · <a href="telemetry.json">Telemetry JSON</a> · <a href="telemetry.tsv">Telemetry TSV</a> · <a href="manifest.json">Manifest JSON</a></nav><ul>${cards}</ul>`);
}

export function renderAll(reportFile,out){
 const inputs=loadInputs(reportFile);inputs.reportFile=reportFile;fs.mkdirSync(out,{recursive:true});writeData(inputs,out);
 const names=circuitOrder.filter(name=>inputs.report.timing?.some(row=>row.case===name));for(const name of [...new Set(inputs.report.timing?.map(row=>row.case)??[])])if(!names.includes(name))names.push(name);
 const probe=spawnSync('gnuplot',['--version'],{encoding:'utf8'}),plotting={available:probe.status===0,reason:probe.status===0?null:'gnuplot is unavailable; JSON and TSV data artifacts were generated'};
 const charts=[];names.forEach((name,index)=>{const file=`${String(index).padStart(2,'0')}_${slug(name)}.png`;const timing=inputs.report.timing.filter(row=>row.case===name);const mutations=inputs.mutations.filter(row=>row.circuit===name);const inventories=inputs.inventories.filter(row=>(row.circuit??row.case)===name);if(plotting.available)renderCase(path.join(out,file),name,index,timing,inventories.length?inventories:mutations);charts.push({case:name,file:plotting.available?file:null,unavailable_reason:plotting.available?null:plotting.reason,timing_cells:timing.filter(row=>row.status==='ok').length,inventory_snapshots:inventories.length,logical_output_snapshots:mutations.length});});
 if(plotting.available)renderOverview(path.join(out,'overview.png'),names,inputs.report);const manifest={schema:1,source_report:path.resolve(reportFile),source_performance:inputs.performanceFile&&path.resolve(inputs.performanceFile),generated_at:new Date().toISOString(),plotting,charts,files:{overview:plotting.available?'overview.png':null,inventory_json:'inventory.json',inventory_tsv:'inventory.tsv',telemetry_json:'telemetry.json',telemetry_tsv:'telemetry.tsv',html_index:'index.html',markdown_index:'index.md'}};fs.writeFileSync(path.join(out,'manifest.json'),JSON.stringify(manifest,null,2)+'\n');writeIndexes(out,manifest);return manifest;
}

if(process.argv[1]===import.meta.filename){const reportFile=path.resolve(process.argv[2]??'report.json');const out=path.resolve(process.argv[3]??path.join(path.dirname(reportFile),'charts'));const manifest=renderAll(reportFile,out);process.stdout.write(`${path.join(out,'index.html')}\n${manifest.charts.length} workloads; ${manifest.plotting.available?'PNG charts generated':manifest.plotting.reason}\n`);}
