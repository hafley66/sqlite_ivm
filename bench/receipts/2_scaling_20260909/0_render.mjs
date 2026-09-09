import fs from 'node:fs';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
const source=path.resolve(process.argv[2]??path.join(import.meta.dirname,"report.json"));
const out=path.resolve(process.argv[3]??import.meta.dirname);
fs.mkdirSync(out,{recursive:true});
const report=JSON.parse(fs.readFileSync(source));
const engines=[
 ['sqlite-ivm','SQLite IVM','#62dba5',7],['dd','Differential Dataflow','#67cfff',9],
 ['prolog','SWI-Prolog','#f9d86a',5],['pg-ivm','Postgres pg_ivm','#ff9970',11],
 ['sqlite-query','SQLite query','#b7ec7b',6],['pg-query','Postgres query','#f66d76',10],
 ['pglite-ivm','PGlite pg_ivm','#97aaff',13],['pglite-query','PGlite query','#eac6a0',12]
];
const panels=[['pipeline','Filter + map'],['join','Join'],['aggregate_churn','Join + grouped sum'],['minmax','Grouped min / max'],['count_distinct','Grouped distinct count'],['reach_cycle','Recursive reachability']].filter(([family])=>report.timing.some(r=>r.case===family&&r.status==='ok'));
const metrics=[['changes_ms','Updates and deletes','Sum of 11 mutation states; initial load and whole-table deletion excluded'],['initial_ms','Initial load','Populate the source tables and compute the first result'],['clear_ms','Whole-table deletion','Delete every remaining row in source table a and maintain the result']];
const q=s=>JSON.stringify(s);
const valid=report.timing.filter(r=>r.status==='ok');
if(!valid.length)throw Error('No validated timing rows');
for(const [metric,title,subtitle] of metrics){
 const values=valid.map(r=>r[metric]);
 if(values.some(v=>!(v>0)))throw Error('Non-positive time cannot be plotted on log scale');
 const lo=10**Math.floor(Math.log10(Math.min(...values))),hi=10**Math.ceil(Math.log10(Math.max(...values)));
 const lines=[`set terminal pngcairo size 2000,1000 noenhanced font "Helvetica,17" background rgb "#101820"`,
 `set output ${q(path.join(out,metric+'.png'))}`,`set multiplot`,
 `set label 100 ${q('IVM SHOOTOUT  /  '+title.toUpperCase())} at screen 0.035,0.958 font ',29' tc rgb '#eef4f8' front`,
 `set label 101 ${q(subtitle)} at screen 0.035,0.921 font ',18' tc rgb '#b3c4d2' front`,
 `set label 102 ${q('Two measured input tiers: 400 and 12,000 rows in a  |  median of 5 runs  |  1 warmup  |  batch = 10, fanout = 10')} at screen 0.035,0.891 font ',16' tc rgb '#b3c4d2' front`];
 engines.forEach(([id,name,color,marker],i)=>{
 const col=i%4,row=Math.floor(i/4),x=.045+col*.244,y=.849-row*.033;
 lines.push(`set label ${110+i} ${q(name)} at screen ${x+.018},${y} tc rgb '${color}' font ',17' front`,
 `set label ${130+i} '' at screen ${x},${y} point pt ${marker} ps 1.4 lc rgb '${color}' front`);
 });
 lines.push(`set label 150 'Read left = less time; read up = more starting input. Lines connect the two measurements only.' at screen .035,.073 tc rgb '#c6d3dc' font ',17' front`,
 `set label 151 'Runtime conditions: SQLite IVM + Postgres use durable commits; DD + Prolog are volatile. PGlite uses NodeFS.' at screen .035,.045 tc rgb '#92a7b8' font ',15' front`,
 `set label 152 'Validated workload timings only. Full run hit disk full; only complete workloads are plotted.' at screen .035,.022 tc rgb '#92a7b8' font ',15' front`,
 `set logscale xy`, `set xrange [${lo}:${hi}]`, `set yrange [220:22000]`,
 `set ytics ('400' 400,'12,000' 12000) nomirror textcolor rgb '#c6d3dc'`,
 `set xtics nomirror textcolor rgb '#c6d3dc'`, `set format x '%g'`, `unset key`, `unset grid`,
 `set border 1 lc rgb '#506779'`, `set tics scale 0`,
 `set xlabel 'Time (ms, log scale)' tc rgb '#b3c4d2' font ',16' offset 0,.2`);
 for(let i=0;i<panels.length;i++){
 const [family,name]=panels[i],col=i%3,row=Math.floor(i/3),left=.075+col*.315,right=left+.250,top=.710-row*.350,bottom=top-.470;
 lines.push(`set lmargin at screen ${left}`,`set rmargin at screen ${right}`,`set tmargin at screen ${top}`,`set bmargin at screen ${bottom}`,
 `set title ${q(name)} tc rgb '#eef4f8' font ',21' offset 0,.4`,
 `set ylabel ${q(col===0?'Starting rows in a':'')} tc rgb '#b3c4d2' font ',16'`,
 `set object 1 rect from graph 0,first 280 to graph 1,first 600 behind fc rgb '#1d2b38' fs solid 1.0 noborder`,
 `set object 2 rect from graph 0,first 8500 to graph 1,first 17000 behind fc rgb '#1d2b38' fs solid 1.0 noborder`);
 const unsupported=report.timing.filter(r=>r.case===family&&r.status==='unsupported').map(r=>r.engine);
 if(unsupported.length) lines.push(`set label 1 'pg_ivm + PGlite pg_ivm: unsupported' at graph .5,-.30 center tc rgb '#92a7b8' font ',13'`);
 else lines.push('unset label 1');
 const plots=[];
 for(const [engine,name,color,marker] of engines){
 const rows=valid.filter(r=>r.case===family&&r.engine===engine).sort((a,b)=>a.rows-b.rows);
 if(!rows.length)continue;
 const dat='$data_'+family+'_'+engine.replaceAll('-','_');
 lines.push(dat+' << EOD',...rows.map(r=>`${r[metric]}\t${r.rows}`),'EOD');
 plots.push(`${dat} using 1:2 with linespoints lw 2.1 pt ${marker} ps 1.6 lc rgb '${color}'`);
 }
 lines.push('plot '+plots.join(', '));
 // Global labels are painted once and then removed to keep text crisp.
 if(i===0)lines.push(...[100,101,102,150,151,152,...engines.map((_,i)=>110+i),...engines.map((_,i)=>130+i)].map(id=>`unset label ${id}`));
 }
 lines.push('unset multiplot');
 const result=spawnSync('gnuplot',[],{input:lines.join('\n')+'\n',encoding:'utf8'});if(result.status!==0)throw Error(result.stderr);
 console.log(path.join(out,metric+'.png'));
}
if(source!==path.join(out,'report.json'))fs.copyFileSync(source,path.join(out,'report.json'));
if(!fs.existsSync(path.join(out,'README.txt')))fs.writeFileSync(path.join(out,'README.txt'),`Source: ${source}\nCommand: just ivm-shootout full\nInput size = initial row count in source a; b and c each start with 7 additional rows.\nMutations change row counts over time and include small graph fixtures after the bulk deletion.\nAll plotted rows passed receipt validation. Unsupported implementations have no point.\nThese are bounded fixtures; two points do not establish asymptotic complexity.\nReport status: ${report.status}\n`);
