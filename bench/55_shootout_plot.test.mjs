import test from 'node:test';
import assert from 'node:assert/strict';
import {parseJsonLines,summarizeSnapshots,summarizeTelemetry} from './55_shootout_plot.mjs';

const metric=(value,unavailable_reason=null)=>({value,unit:'bytes',unavailable_reason});
const snapshot=(state,bytes,rows)=>({
 state,output_rows:rows,state_inventory:{
  summary:{table_count:metric(2),index_count:metric(null,'engine has no indexes'),native_collection_count:metric(1),total_rows:metric(rows),table_bytes:metric(bytes),index_bytes:metric(null,'not measured'),total_relation_bytes:metric(bytes)},
  storage:{database_file_bytes:metric(bytes*2),wal_file_bytes:metric(0),database_allocated_bytes:metric(bytes*2)},
  process_memory:{peak_rss_bytes:metric(bytes*3)},
 },
});

test('JSONL parser reports the source line',()=>{
 assert.deepEqual(parseJsonLines('{"event":"ok"}\n\n'),[{event:'ok'}]);
 assert.throws(()=>parseJsonLines('{"event":"ok"}\n{broken}','fixture.jsonl'),/fixture\.jsonl:2: invalid JSON/);
});

test('snapshot summaries preserve null sensors and the state for each independent peak',()=>{
 const summary=summarizeSnapshots([snapshot('initial',100,4),snapshot('change',300,9),snapshot('final',200,2)]);
 assert.deepEqual({initial:summary.initial.state,final:summary.final.state,bytes:summary.peaks.total_relation_bytes,rows:summary.peaks.total_rows,index:summary.peaks.index_bytes},{initial:'initial',final:'final',bytes:{value:300,state:'change',partial:false},rows:{value:9,state:'change',partial:false},index:null});
});

test('telemetry derives distribution samples and logical output throughput from measured states',()=>{
 const summary=summarizeTelemetry([{update_plus_query_ms:10,output_rows:2},{update_plus_query_ms:20,output_rows:8},{update_plus_query_ms:null,output_rows:100}]);
 assert.deepEqual({sample_count:summary.sample_count,latency_ms:summary.latency_ms,logical_output_rows_per_second:summary.logical_output_rows_per_second},{sample_count:2,latency_ms:{p50:10,p95:20,p99:20},logical_output_rows_per_second:{p50:200,p95:400,p99:400}});
 assert.deepEqual({sample_count:summary.phases.changes.sample_count,latency_ms:summary.phases.changes.latency_ms},{sample_count:1,latency_ms:{p50:30,p95:30,p99:30}});
});
