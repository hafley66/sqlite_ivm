export const metric=(value,unit,unavailable_reason=null)=>({value:value===null?null:Number(value),unit,unavailable_reason});
const statisticNames=['blks_read','blks_hit','tup_returned','tup_fetched','tup_inserted','tup_updated','tup_deleted','xact_commit','xact_rollback','temp_files','temp_bytes'];
const statisticUnit=name=>name==='temp_bytes'?'bytes':name==='temp_files'?'files':name.startsWith('blks_')?'blocks':name.startsWith('tup_')?'tuples':'transactions';
export async function postgresStatistics(db,baseline=null) {
 if(baseline?.unavailable_reason)return {scope:'current database',source:'pg_stat_database',physical_disk_io:false,counters:Object.fromEntries(statisticNames.map(name=>[name,metric(null,statisticUnit(name),baseline.unavailable_reason)])),page_bytes:null,page_bytes_unavailable_reason:baseline.unavailable_reason};
 try {
  try{await db.query('SELECT pg_stat_force_next_flush()');}catch{}
  try{await db.query('SELECT pg_stat_clear_snapshot()');}catch{}
  const row=(await db.query(`SELECT ${statisticNames.join(',')} FROM pg_stat_database WHERE datname=current_database()`)).rows[0];
  if(!row)throw Error('current database is absent from pg_stat_database');
  const counters=Object.fromEntries(statisticNames.map(name=>[name,Number(row[name])]));
  if(!baseline)return {counters};
  return {scope:'current database since adapter baseline',source:'pg_stat_database cumulative database page-cache and tuple counters',physical_disk_io:false,page_bytes:null,page_bytes_unavailable_reason:'pg_stat_database block counters do not report transferred bytes',counters:Object.fromEntries(statisticNames.map(name=>[name,metric(counters[name]-baseline.counters[name],statisticUnit(name))]))};
 } catch(error) {
  return baseline?{scope:'current database',source:'pg_stat_database',physical_disk_io:false,counters:Object.fromEntries(statisticNames.map(name=>[name,metric(null,statisticUnit(name),`pg_stat_database unavailable: ${error.message}`)])),page_bytes:null,page_bytes_unavailable_reason:`pg_stat_database unavailable: ${error.message}`}:{unavailable_reason:`pg_stat_database unavailable: ${error.message}`};
 }
}
const quoted=name=>'"'+name.replaceAll('"','""')+'"';
export async function postgresInventory(db,{ivm,embedded}) {
 const limitations=[];const relations=[];
 let catalog;
 try{catalog=(await db.query(`SELECT c.relname AS name,c.relkind AS kind,p.relname AS parent_name,pg_relation_size(c.oid) AS heap_bytes,pg_indexes_size(c.oid) AS owned_index_bytes,pg_total_relation_size(c.oid) AS total_bytes FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace LEFT JOIN pg_index ix ON ix.indexrelid=c.oid LEFT JOIN pg_class p ON p.oid=ix.indrelid WHERE n.nspname='public' AND c.relkind IN ('r','i','m') ORDER BY c.relkind,c.relname`)).rows;}
 catch(error){return unavailableInventory(`PostgreSQL/PGlite relation-size functions unavailable: ${error.message}`,embedded);}
 for(const row of catalog){
  const kind=row.kind==='i'?'index':'table'; const source=['a','b','c'].includes(kind==='index'?row.parent_name:row.name);
  const result=ivm&&(kind==='index'?row.parent_name:row.name)==='circuit_view'; if(!source&&!result&&!row.name.startsWith('__ivm_'))continue;
  const role=source?(kind==='index'?'source-index':'source'):result?'result':kind==='index'?'support-index':'support';
  let count=null,reason=null;if(kind==='table'){try{count=(await db.query(`SELECT count(*) AS n FROM public.${quoted(row.name)}`)).rows[0].n;}catch(error){reason=`row count unavailable: ${error.message}`;}}
  const heap=Number(row.heap_bytes),ownedIndex=Number(row.owned_index_bytes),total=Number(row.total_bytes);
  relations.push({name:row.name,kind,role,counted_in_totals:true,row_count:metric(count,'rows',reason??(kind==='index'?'row count does not apply to an index':null)),bytes:{allocated:metric(kind==='index'?heap:total-ownedIndex,'bytes'),data:metric(kind==='index'?null:heap,'bytes',kind==='index'?'index has no table heap bytes':null),index:metric(kind==='index'?heap:ownedIndex,'bytes')},toast_bytes:metric(kind==='table'?Math.max(0,total-heap-ownedIndex):null,'bytes',kind==='index'?'TOAST does not apply to an index':null)});
 }
 if(!ivm)limitations.push('plain query has no durable result relation; output_bytes is transient serialized query output');
 const tables=relations.filter(r=>r.kind==='table'),indexes=relations.filter(r=>r.kind==='index');
 const rows=tables.map(r=>r.row_count.value);const database=(await db.query('SELECT pg_database_size(current_database()) AS bytes')).rows[0].bytes;
 const tableBytes=tables.reduce((n,r)=>n+r.bytes.allocated.value,0),indexBytes=indexes.reduce((n,r)=>n+r.bytes.allocated.value,0);
 const roleMetric=role=>{const values=tables.filter(r=>r.role===role).map(r=>r.row_count.value);return values.some(x=>x===null)?{...metric(null,'rows','one or more role row counts unavailable'),partial:true,known_value:values.filter(x=>x!==null).reduce((a,b)=>a+b,0)}:{...metric(values.reduce((a,b)=>a+b,0),'rows'),partial:false};};
 return {schema_version:1,measured_at:'after-output-validation',outside_timed_region:true,scope:'public source relations and their indexes plus maintained-result relations and their indexes; pg_ivm extension-schema metadata is excluded',relations,summary:{table_count:metric(tables.length,'tables'),index_count:metric(indexes.length,'indexes'),native_collection_count:metric(null,'collections','SQL adapter has no native collection inventory'),total_rows:rows.some(x=>x===null)?{...metric(null,'rows','one or more included table row counts unavailable'),partial:true,known_value:rows.filter(x=>x!==null).reduce((a,b)=>a+b,0)}:{...metric(rows.reduce((a,b)=>a+b,0),'rows'),partial:false},rows_by_role:Object.fromEntries([...new Set(tables.map(r=>r.role))].map(role=>[role,roleMetric(role)])),table_bytes:metric(tableBytes,'bytes'),index_bytes:metric(indexBytes,'bytes'),total_relation_bytes:metric(tableBytes+indexBytes,'bytes')},storage:{database_file_bytes:metric(null,'bytes','server database may span multiple files; pg_database_size is reported separately'),wal_file_bytes:metric(null,'bytes','WAL is cluster-wide and cannot be assigned to this case database snapshot'),database_allocated_bytes:metric(database,'bytes'),database_size_scope:'pg_database_size(current_database()) is database-wide and overhead-inclusive; it is not maintained-view size'},process_memory:{rss_bytes:metric(null,'bytes','measured by the parent runner as process-group or process peak RSS, outside this snapshot')},limitations:[...limitations,'pg_ivm extension-schema metadata is outside the per-view physical relation inventory']};
}
function unavailableInventory(reason,embedded){return {schema_version:1,measured_at:'after-output-validation',outside_timed_region:true,scope:'PostgreSQL relation inventory unavailable',relations:[],summary:{table_count:metric(null,'tables',reason),index_count:metric(null,'indexes',reason),native_collection_count:metric(null,'collections','SQL adapter has no native collections'),total_rows:{...metric(null,'rows',reason),partial:false},rows_by_role:{},table_bytes:metric(null,'bytes',reason),index_bytes:metric(null,'bytes',reason),total_relation_bytes:metric(null,'bytes',reason)},storage:{database_file_bytes:metric(null,'bytes',reason),wal_file_bytes:metric(null,'bytes','WAL is cluster-wide'),database_allocated_bytes:metric(null,'bytes',reason),database_size_scope:embedded?'PGlite relation-size support unavailable':'PostgreSQL relation-size support unavailable'},process_memory:{rss_bytes:metric(null,'bytes','measured by parent runner')},limitations:[reason]};}
