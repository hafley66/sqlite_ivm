import { createHash } from 'node:crypto';

// SQL is fixed lab source. Fixture values are bounded integers; no consumer SQL
// or identifiers are interpolated here. This oracle does not execute SQL.
export const circuits = {
  pipeline: 'SELECT k AS c0,v*2 AS c1 FROM a WHERE v>=0',
  fanout_fanin: 'SELECT k AS c0,v AS c1 FROM a WHERE v>=0 UNION ALL SELECT k AS c0,v AS c1 FROM a WHERE v%2=0',
  distinct: 'SELECT DISTINCT k AS c0,v AS c1 FROM a',
  join: 'SELECT a.k AS c0,a.v*b.v AS c1 FROM a JOIN b ON a.k=b.k',
  self_join: 'SELECT x.k AS c0,y.v AS c1 FROM a x JOIN a y ON x.v=y.k',
  chain: 'SELECT a.k AS c0,c.v AS c1 FROM a JOIN b ON a.v=b.k JOIN c ON b.v=c.k',
  diamond: 'SELECT a.k AS c0,b.v AS c1 FROM a JOIN b ON a.v=b.k UNION ALL SELECT a.k AS c0,c.v AS c1 FROM a JOIN c ON a.v=c.k',
  semijoin: 'SELECT a.k AS c0,a.v AS c1 FROM a WHERE EXISTS (SELECT 1 FROM b WHERE b.k=a.k)',
  antijoin: 'SELECT a.k AS c0,a.v AS c1 FROM a WHERE NOT EXISTS (SELECT 1 FROM b WHERE b.k=a.k)',
  aggregate_churn: 'SELECT a.k AS c0,COUNT(*) AS c1,SUM(a.v*b.v) AS c2 FROM a JOIN b ON a.k=b.k GROUP BY a.k',
  reach_cycle: 'WITH RECURSIVE reachable(node) AS (SELECT k FROM b UNION SELECT a.v FROM a JOIN reachable r ON a.k=r.node) SELECT node AS c0 FROM reachable',
};
export const sortRows = rows => rows.toSorted((a,b) => {for(let i=0;i<a.length;i++) if(a[i]!==b[i]) return a[i]-b[i]; return 0;});
export const digest = text => createHash('sha256').update(text).digest('hex');
export const outputText = rows => sortRows(rows).map(r=>`S\t${r.join('\t')}\n`).join('');
export const inputText = tables => ['a','b','c'].map(t=>sortRows(tables[t]).map(r=>`${t.toUpperCase()}\t${r.join('\t')}\n`).join('')).join('');
export function circuitOracle(family,{a,b,c}) {
  const project = rows => rows.map(([,k,v])=>[k,v]);
  const join = (left,right,predicate,project) => left.flatMap(x=>right.filter(y=>predicate(x,y)).map(y=>project(x,y)));
  switch(family) {
    case 'pipeline': return a.filter(r=>r[2]>=0).map(([,k,v])=>[k,v*2]);
    case 'fanout_fanin': return project([...a.filter(r=>r[2]>=0),...a.filter(r=>r[2]%2===0)]);
    case 'distinct': return [...new Map(project(a).map(r=>[JSON.stringify(r),r])).values()];
    case 'join': return join(a,b,(x,y)=>x[1]===y[1],(x,y)=>[x[1],x[2]*y[2]]);
    case 'self_join': return join(a,a,(x,y)=>x[2]===y[1],(x,y)=>[x[1],y[2]]);
    case 'chain': return a.flatMap(x=>b.filter(y=>x[2]===y[1]).flatMap(y=>c.filter(z=>y[2]===z[1]).map(z=>[x[1],z[2]])));
    case 'diamond': return join(a,[...b,...c],(x,y)=>x[2]===y[1],(x,y)=>[x[1],y[2]]);
    case 'semijoin': return project(a.filter(x=>b.some(y=>x[1]===y[1])));
    case 'antijoin': return project(a.filter(x=>!b.some(y=>x[1]===y[1])));
    case 'aggregate_churn': {
      const groups = new Map();
      for(const [key,value] of circuitOracle('join',{a,b,c})) {const g=groups.get(key)??[key,0,0];g[1]++;g[2]+=value;groups.set(key,g);}
      return [...groups.values()];
    }
    case 'reach_cycle': {
      const reached=new Set(b.map(r=>r[1])); let changed=true;
      while(changed){changed=false;for(const [,k,v] of a)if(reached.has(k)&&!reached.has(v)){reached.add(v);changed=true;}}
      return [...reached].map(k=>[k]);
    }
    default: throw new Error(`unknown circuit ${family}`);
  }
}
export function makeCircuitFixture(family,rows=24,batch=3,fanout=4) {
  if(!Object.hasOwn(circuits,family))throw new Error(`unknown circuit ${family}`);
  if(![rows,batch,fanout].every(Number.isSafeInteger)||rows<1||rows>12000||batch<1||batch>rows||fanout<1)throw new Error('bounded fixture dimensions required');
  const tables={a:[],b:[],c:[]}; const states=[];
  function state(name,writes){
    const sql=[];
    for(const {table,id,row} of writes){
      tables[table]=tables[table].filter(r=>r[0]!==id);
      sql.push(`DELETE FROM ${table} WHERE id=${id};`);
      if(row){if(!row.every(n=>Number.isSafeInteger(n)&&Math.abs(n)<=1000000))throw new Error('integer bounds');tables[table].push(row);sql.push(`INSERT INTO ${table}(id,k,v) VALUES(${row.join(',')});`);}
    }
    const inputs=structuredClone(tables);const output=sortRows(circuitOracle(family,inputs));
    states.push({name,writes,mutation_sql:sql.join('\n'),inputs,input_hash:digest(inputText(inputs)),expected:{rows:output,checksum:digest(outputText(output))}});
  }
  const initial=[];
  for(let id=1;id<=rows;id++)initial.push({table:'a',id,row:[id,Math.floor((id-1)/fanout),id%7-3]});
  for(const table of ['b','c'])for(let id=1;id<=7;id++)initial.push({table,id,row:[id,id-4,table==='b'?id%3-1:id%4]});
  state('initial',initial);
  state('duplicate_support',[{table:'a',id:rows+1,row:[rows+1,...tables.a[0].slice(1)]}]);
  state('retract_one_support',[{table:'a',id:1,row:null}]);
  state('batch_key_value_move',Array.from({length:batch},(_,i)=>({table:'a',id:i+2,row:[i+2,0,-i%3]})));
  state('right_support_move',[{table:'b',id:1,row:[1,0,0]},{table:'b',id:2,row:[2,0,-2]}]);
  state('third_side_move',[{table:'c',id:1,row:[1,0,-3]}]);
  state('clear_roots',tables.b.map(r=>({table:'b',id:r[0],row:null})));
  state('clear_edges',tables.a.map(r=>({table:'a',id:r[0],row:null})));
  state('cycle_seed',[{table:'b',id:1,row:[1,1,1]},...[ [1,1,2],[2,2,3],[3,3,2],[4,1,4],[5,4,3] ].map(row=>({table:'a',id:row[0],row}))]);
  state('diamond_path_retract',[{table:'a',id:2,row:null}]);
  state('root_retract',[{table:'b',id:1,row:null}]);
  state('root_restore',[{table:'b',id:1,row:[1,1,1]}]);
  state('cycle_break',[{table:'a',id:3,row:null}]);
  return {circuit:family,query:circuits[family],rows,batch_size:batch,fanout,states};
}
