import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import crypto from 'node:crypto';
import {spawn,spawnSync} from 'node:child_process';
import {engines,arms,circuitNames,featureNames,ddContracts,writeReport} from './51_shootout_report.mjs';
import {renderAll} from './55_shootout_plot.mjs';
const root=path.resolve(import.meta.dirname,'..'),repo=path.dirname(root);
const args=process.argv.slice(2);const profile=args[0]&&!args[0].startsWith('--')?args.shift():'quick';
const option=(name,fallback)=>{const i=args.indexOf('--'+name);if(i<0)return fallback;if(!args[i+1]||args[i+1].startsWith('--'))throw new Error(`--${name} needs a value`);return args[i+1];};
if(args.includes('--help')){console.log('just ivm-shootout [smoke|quick|full] [--engines sqlite-ivm,dd,prolog,pg-ivm,pg-query,sqlite-query,pglite-ivm,pglite-query] [--out DIR] [--details] [--color]\nsmoke: all semantics, one measured 24-row run. quick: all semantics, 1 warmup + 3 measured 400-row runs. full: all semantics, 1 warmup + 5 measured 400/12000-row runs of pipeline/join/aggregate_churn/reach_cycle/minmax/count_distinct.\nExit 0 = checked requested adapters passed (unsupported query shapes remain visible); 1 = observed result mismatch; 2 = missing dependency, missing receipt, timeout or other execution failure.');process.exit(0);}
for(let i=0;i<args.length;i++){if(['--out','--engines'].includes(args[i]))i++;else if(!['--details','--color'].includes(args[i]))throw new Error(`unknown option ${args[i]}`);}
if(!['smoke','quick','full'].includes(profile))throw new Error('profile must be smoke, quick, or full');
const requested=option('engines',engines.join(',')).split(',');if(!requested.length||new Set(requested).size!==requested.length||requested.some(e=>!engines.includes(e)))throw new Error('unknown or duplicate engine');
const directory=path.resolve(option('out',path.join(root,'bench/results',`shootout-${new Date().toISOString().replaceAll(':','-')}-${process.pid}`)));
if(fs.existsSync(directory))throw new Error(`refusing to overwrite ${directory}`);fs.mkdirSync(directory,{recursive:true});fs.mkdirSync(path.join(directory,'logs'));
const env={...process.env,IVM_COMPACT_ARTIFACTS:process.env.IVM_KEEP_DATABASES==='1'?'0':'1'};
// Prefer the installed SQLite matching the release extension host on macOS.
if(process.platform==='darwin'&&!env.SQLITE3_LIB_DIR){const brew=spawnSync('brew',['--prefix','sqlite'],{encoding:'utf8'});if(brew.status===0){const prefix=brew.stdout.trim();env.SQLITE3_LIB_DIR=path.join(prefix,'lib');env.SQLITE3_INCLUDE_DIR=path.join(prefix,'include');env.SQLITE3??=path.join(prefix,'bin/sqlite3');}}
const revision=spawnSync('git',['rev-parse','HEAD'],{cwd:repo,encoding:'utf8'}).stdout?.trim()??null;
const manifest={schema:1,profile,started_at:new Date().toISOString(),engines:requested,availability:{},phases:[],revision,sources:{},artifacts:env.IVM_COMPACT_ARTIFACTS==='1'?'shared fixtures and receipts; successful per-case databases discarded':'full per-case databases retained',performance:{repetitions:profile==='smoke'?1:profile==='quick'?3:5,warmups:profile==='smoke'?0:1}};
for(const base of ['src','tests','examples','bench','scripts']){
 const walk=dir=>{for(const entry of fs.readdirSync(dir,{withFileTypes:true})){if(['target','results','receipts','node_modules','.work'].includes(entry.name))continue;const p=path.join(dir,entry.name);if(entry.isDirectory())walk(p);else if(/\.(rs|mjs|py|pl|sh|json|toml|lock)$/.test(p))manifest.sources[path.relative(root,p)]=crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');}};walk(path.join(root,base));
}
const save=()=>fs.writeFileSync(path.join(directory,'run.json'),JSON.stringify(manifest,null,2)+'\n');
const executable=command=>spawnSync(command,['--version'],{env,encoding:'utf8',timeout:10000}).status===0;
let activeChild=null;
function killChild(child,signal){if(!child?.pid)return;try{process.kill(process.platform==='win32'?child.pid:-child.pid,signal);}catch(e){if(e.code!=='ESRCH')throw e;}}

async function run(name,command,argv,{receipt,timeout=600000}={}){
 const log=path.join(directory,'logs',name+'.log');process.stdout.write(`[${name}] ${path.relative(directory,log)}\n`);
 const fd=fs.openSync(log,'w');const out=receipt?fs.openSync(path.join(directory,receipt),'w'):null;const started=Date.now();let error=null,timedOut=false;
 const status=await new Promise(resolve=>{
  const child=spawn(command,argv,{cwd:repo,env,stdio:['ignore','pipe','pipe'],detached:process.platform!=='win32'});activeChild=child;
  child.stdout.on('data',chunk=>{fs.writeSync(fd,chunk);if(out!==null)fs.writeSync(out,chunk);});child.stderr.on('data',chunk=>fs.writeSync(fd,chunk));
  const timer=setTimeout(()=>{timedOut=true;killChild(child,'SIGTERM');setTimeout(()=>killChild(child,'SIGKILL'),1000).unref();},timeout);
  child.on('error',e=>{error=e.message;});child.on('close',(code,signal)=>{clearTimeout(timer);activeChild=null;resolve(code??(signal?124:127));});
 });fs.closeSync(fd);if(out!==null)fs.closeSync(out);
 manifest.phases.push({name,command:[command,...argv],status,timeout:timedOut,error,wall_ms:Date.now()-started,log});save();return status;
}
let cluster=null,prefix=null;
function stopCluster(){if(cluster){spawnSync(path.join(prefix,'bin/pg_ctl'),['-D',path.join(cluster,'data'),'-m','immediate','stop'],{env,stdio:'ignore',timeout:15000});fs.rmSync(cluster,{recursive:true,force:true});cluster=null;}}
for(const signal of ['SIGINT','SIGTERM'])process.on(signal,()=>{killChild(activeChild,'SIGTERM');stopCluster();manifest.finished_at=new Date().toISOString();manifest.interrupted=signal;save();process.exit(130);});
try{
 const needsNative=requested.some(e=>['sqlite-ivm','sqlite-query'].includes(e));const needsDd=requested.includes('dd');
 // Exporting the shared typed fixtures requires the native query oracle even for a DD-only selection.
 const nativeReady=await run('build-native-consumers','cargo',['build','--locked','--release','--features','bench','--examples','--manifest-path',path.join(root,'Cargo.toml')])===0;
 const extensionReady=nativeReady&&await run('build-extension','cargo',['build','--locked','--release','--features','extension','--manifest-path',path.join(root,'Cargo.toml')])===0;
 const ddReady=!needsDd||await run('build-dd','cargo',['build','--locked','--release','--manifest-path',path.join(root,'bench/Cargo.toml')])===0;
 const prologReady=!requested.includes('prolog')||executable('swipl');
 const sqlEngines=requested.some(e=>['pg-ivm','pg-query','pglite-ivm','pglite-query'].includes(e));
 let npmReady=fs.existsSync(path.join(root,'bench/shared/node_modules/pg'))&&fs.existsSync(path.join(root,'bench/shared/node_modules/@electric-sql/pglite'));
 if(sqlEngines&&!npmReady)npmReady=await run('install-node-dependencies','npm',['ci','--ignore-scripts','--prefix',path.join(root,'bench/shared')])===0;
 const nativePg=requested.some(e=>['pg-ivm','pg-query'].includes(e));let pgReady=false;
 if(nativePg&&npmReady){
  const candidates=[env.IVM_POSTGRES_PREFIX,path.join(root,'bench/shared/.work/postgres-18.6')].filter(Boolean);
  const config=spawnSync('pg_config',['--bindir'],{encoding:'utf8'});if(config.status===0)candidates.push(path.dirname(config.stdout.trim()));
  // Reuse an existing task-local installation; its path is recorded in run.json.
  const common=spawnSync('git',['rev-parse','--path-format=absolute','--git-common-dir'],{cwd:repo,encoding:'utf8'}).stdout?.trim();
  if(common)candidates.push(path.join(path.dirname(common),'.boop-worktrees/feature/postgres-pglite-ivm/v6/labs/exec_shootout/postgres_pglite_ivm/.work/postgres-18.6'));
  prefix=candidates.find(p=>{const c=spawnSync(path.join(p,'bin/pg_config'),['--sharedir'],{encoding:'utf8'});return c.status===0&&fs.existsSync(path.join(c.stdout.trim(),'extension/pg_ivm.control'));});
  if(!prefix){manifest.postgres_setup='No PostgreSQL with pg_ivm found. Set IVM_POSTGRES_PREFIX or run bash sqlite_ivm/bench/shared/1_prepare_native.sh.';}
  else{
   manifest.postgres_prefix=prefix;cluster=fs.mkdtempSync('/tmp/ivm-shootout.');fs.mkdirSync(path.join(cluster,'socket'));
   env.PGHOST=path.join(cluster,'socket');env.PGPORT='5432';env.PGUSER=os.userInfo().username;env.PGDATABASE='postgres';env.PGDATABASE_NATIVE_QUERY='crossover_query';env.PGDATABASE_NATIVE_IVM='crossover_ivm';
   if(await run('postgres-init',path.join(prefix,'bin/initdb'),['-D',path.join(cluster,'data'),'--auth=trust','--no-locale','--encoding=UTF8'])===0){
    const settings=`-c listen_addresses='' -c unix_socket_directories='${env.PGHOST}' -c shared_preload_libraries='pg_ivm' -c shared_buffers=32MB -c work_mem=1MB -c temp_file_limit=128MB -c statement_timeout=120000 -c max_connections=16 -c fsync=on -c synchronous_commit=on -c full_page_writes=on`;
    pgReady=await run('postgres-start',path.join(prefix,'bin/pg_ctl'),['-D',path.join(cluster,'data'),'-l',path.join(directory,'logs/postgres-server.log'),'-o',settings,'start','-w'])===0;
    if(pgReady){env.IVM_POSTMASTER_PID=fs.readFileSync(path.join(cluster,'data/postmaster.pid'),'utf8').split('\n')[0];for(const database of [env.PGDATABASE_NATIVE_QUERY,env.PGDATABASE_NATIVE_IVM])pgReady=await run('postgres-create-'+database,path.join(prefix,'bin/createdb'),[database])===0&&pgReady;}
   }
  }
 }
 for(const engine of requested){const available=engine==='dd'?ddReady:engine==='prolog'?prologReady:engine.startsWith('pg-')?pgReady:engine.startsWith('pglite-')?npmReady:extensionReady;manifest.availability[engine]={available,reason:available?null:engine==='prolog'?'Install SWI-Prolog (swipl)':engine.startsWith('pg-')?manifest.postgres_setup??'PostgreSQL startup failed; inspect logs':'Build/dependency setup failed; inspect logs'};}save();
 const featuresDir=path.join(directory,'features');fs.mkdirSync(featuresDir);
 const library=path.join(root,'target/release',process.platform==='darwin'?'libsqlite_ivm.dylib':'libsqlite_ivm.so');
 if(extensionReady){
  const exportStatus=await run('features-native',path.join(root,'target/release/examples/5_feature_case'),[path.join(root,'tests/fixtures/1_features.json'),library,path.join(featuresDir,'cases')],{receipt:'features/native.jsonl'});
  if(exportStatus===0){
   if(needsDd&&ddReady)await run('features-dd',process.execPath,[path.join(root,'bench/42_feature_run.mjs'),'dd',path.join(featuresDir,'cases'),path.join(root,'bench/target/release/feature_dd')],{receipt:'features/dd.jsonl'});
   if(requested.includes('prolog')&&prologReady)await run('features-prolog','swipl',['-q','-s',path.join(root,'bench/50_feature_prolog.pl'),'--',path.join(featuresDir,'cases')],{receipt:'features/prolog.jsonl'});
   if(pgReady)await run('features-pg',process.execPath,[path.join(root,'bench/42_feature_run.mjs'),'pg',path.join(featuresDir,'cases')],{receipt:'features/pg.jsonl'});
   if(npmReady&&requested.some(e=>e.startsWith('pglite-')))await run('features-pglite',process.execPath,[path.join(root,'bench/42_feature_run.mjs'),'pglite',path.join(featuresDir,'cases'),path.join(directory,'pglite-features')],{receipt:'features/pglite.jsonl'});
  }
 }
 if(needsDd&&ddReady)await run('dd-contracts',path.join(root,'bench/target/release/dd_contracts'),[],{receipt:'dd-contracts.jsonl',timeout:60000});
 if(needsNative&&extensionReady)await run('native-value-transactions','bash',[path.join(root,'scripts/14_native_values.sh'),library]);
 const live=requested.filter(e=>manifest.availability[e].available);
 const circuitArgs=['--profile','circuits','--arms',live.map(e=>arms[e]).join(','),'--sqlite-bin',path.join(root,'target/release/examples/4_sqlite_case'),'--sqlite-extension',library,'--circuit-dd-bin',path.join(root,'bench/target/release/circuit_dd'),'--semantic-dd-bin',path.join(root,'bench/target/release/semantic_dd')];
 if(live.length){
  env.IVM_RUN_ROOT=path.join(directory,'circuit-artifacts');
  await run('shared-circuit-semantics',process.execPath,[path.join(root,'bench/shared/12_crossover_runner.mjs'),...circuitArgs,'--output',path.join(directory,'circuits.jsonl'),'--warmups','0','--repetitions','1'],{timeout:1200000});
  env.IVM_RUN_ROOT=path.join(directory,'performance-artifacts');
  const perf=profile==='smoke'?['--circuit-grid','small']:['--circuit-grid','12k','--circuit-cells',profile==='quick'?'400:10:10':'400:10:10,12000:10:10','--circuits',profile==='quick'?circuitNames.join(','):'pipeline,join,aggregate_churn,reach_cycle,minmax,count_distinct'];
  await run('performance',process.execPath,[path.join(root,'bench/shared/12_crossover_runner.mjs'),...circuitArgs,'--output',path.join(directory,'performance.jsonl'),'--warmups',String(manifest.performance.warmups),'--repetitions',String(manifest.performance.repetitions),...perf],{timeout:1200000});
 }
}catch(error){manifest.phases.push({name:'orchestrator',status:2,error:error.stack,log:path.join(directory,'run.json')});}
finally{stopCluster();manifest.finished_at=new Date().toISOString();save();}
const report=writeReport(directory,{details:args.includes('--details'),color:(process.stdout.isTTY&&!env.NO_COLOR)||args.includes('--color')});
const chartDirectory=path.join(directory,'charts');renderAll(path.join(directory,'report.json'),chartDirectory);
process.stdout.write(`Chart index: ${path.join(chartDirectory,'index.html')}\nTelemetry TSV: ${path.join(chartDirectory,'telemetry.tsv')}\nInventory TSV: ${path.join(chartDirectory,'inventory.tsv')}\n`);
process.exitCode=report.status==='ok'?0:report.status==='mismatch'?1:2;
