import { createHash } from 'node:crypto';

// SQL is fixed lab source. No consumer SQL or identifiers are interpolated here.
// This oracle does not execute SQL.
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
export const valueDomains={
  integers:{k_sql:'INTEGER NOT NULL',v_sql:'INTEGER NOT NULL'},
  text_nocase:{k_sql:'TEXT NOT NULL COLLATE NOCASE',v_sql:'INTEGER NOT NULL'},
  mixed_int_real:{k_sql:'NOT NULL',v_sql:'INTEGER NOT NULL'},
};
const compareCell=(a,b)=>typeof a==='number'&&typeof b==='number'?a-b:String(a)<String(b)?-1:String(a)>String(b)?1:0;
export const sortRows = rows => rows.toSorted((a,b) => {for(let i=0;i<a.length;i++){const order=compareCell(a[i],b[i]);if(order)return order;}return 0;});
export const digest = text => createHash('sha256').update(text).digest('hex');
export const outputText = rows => sortRows(rows).map(r=>`S\t${r.join('\t')}\n`).join('');
export const inputText = tables => ['a','b','c'].map(t=>sortRows(tables[t]).map(r=>`${t.toUpperCase()}\t${r.join('\t')}\n`).join('')).join('');
const domainKey=(value,domain)=>domain==='text_nocase'?String(value).toLowerCase():domain==='mixed_int_real'?Number(value):value;
const sameKey=(left,right,domain)=>domainKey(left,domain)===domainKey(right,domain);
const valueRow=(domain,[id,k,v],real=false,caseVariant=false)=>domain==='text_nocase'?[id,`${caseVariant&&id%2===0?'N':'n'}${String(k).replace('-', '_')}`,v]:domain==='mixed_int_real'?[id,real?`${k}.0`:k,v]:[id,k,v];
const sqlCell=(value,domain)=>typeof value==='number'?String(value):domain==='mixed_int_real'&&/^-?\d+(?:\.0)?$/.test(value)?value:String.raw`'${value.replaceAll("'","''")}'`;
export function circuitOracle(family,{a,b,c},domain='integers') {
  const project = rows => rows.map(([,k,v])=>[k,v]);
  const join = (left,right,predicate,project) => left.flatMap(x=>right.filter(y=>predicate(x,y)).map(y=>project(x,y)));
  switch(family) {
    case 'pipeline': return a.filter(r=>r[2]>=0).map(([,k,v])=>[k,v*2]);
    case 'fanout_fanin': return project([...a.filter(r=>r[2]>=0),...a.filter(r=>r[2]%2===0)]);
    case 'distinct': return [...new Map(project(a).map(r=>[JSON.stringify([domainKey(r[0],domain),r[1]]),r])).values()];
    case 'join': return join(a,b,(x,y)=>sameKey(x[1],y[1],domain),(x,y)=>[x[1],x[2]*y[2]]);
    case 'self_join': return join(a,a,(x,y)=>sameKey(x[2],y[1],domain),(x,y)=>[x[1],y[2]]);
    case 'chain': return a.flatMap(x=>b.filter(y=>sameKey(x[2],y[1],domain)).flatMap(y=>c.filter(z=>sameKey(y[2],z[1],domain)).map(z=>[x[1],z[2]])));
    case 'diamond': return join(a,[...b,...c],(x,y)=>sameKey(x[2],y[1],domain),(x,y)=>[x[1],y[2]]);
    case 'semijoin': return project(a.filter(x=>b.some(y=>sameKey(x[1],y[1],domain))));
    case 'antijoin': return project(a.filter(x=>!b.some(y=>sameKey(x[1],y[1],domain))));
    case 'aggregate_churn': {
      const groups = new Map();
      for(const [key,value] of circuitOracle('join',{a,b,c},domain)) {const normalized=domainKey(key,domain);const g=groups.get(normalized)??[key,0,0];g[1]++;g[2]+=value;groups.set(normalized,g);}
      return [...groups.values()];
    }
    case 'reach_cycle': {
      const reached=[]; const add=value=>{if(reached.some(entry=>sameKey(entry,value,domain)))return false;reached.push(value);return true;};
      for(const row of b)add(row[1]); let remaining=a.length+1;
      while(remaining-->0){let changed=false;for(const [,k,v] of a)if(reached.some(entry=>sameKey(entry,k,domain))&&add(v))changed=true;if(!changed)return reached.map(k=>[k]);}
      throw new Error('reach_cycle iteration budget exhausted');
    }
    default: throw new Error(`unknown circuit ${family}`);
  }
}
export function makeCircuitFixture(family,rows=24,batch=3,fanout=4,domain='integers') {
  if(!Object.hasOwn(circuits,family))throw new Error(`unknown circuit ${family}`);
  if(!Object.hasOwn(valueDomains,domain))throw new Error(`unknown value domain ${domain}`);
  if(![rows,batch,fanout].every(Number.isSafeInteger)||rows<1||rows>12000||batch<1||batch>rows||fanout<1)throw new Error('bounded fixture dimensions required');
  const tables={a:[],b:[],c:[]}; const states=[];
  const cycleVariants=family==='reach_cycle';
  function state(name,writes){
    const sql=[];
    for(const {table,id,row} of writes){
      tables[table]=tables[table].filter(r=>r[0]!==id);
      sql.push(`DELETE FROM ${table} WHERE id=${id};`);
      if(row){if(!Number.isSafeInteger(row[0])||Math.abs(row[0])>1000000||!Number.isSafeInteger(row[2])||Math.abs(row[2])>1000000)throw new Error('fixture bounds');tables[table].push(row);sql.push(`INSERT INTO ${table}(id,k,v) VALUES(${row.map(value=>sqlCell(value,domain)).join(',')});`);}
    }
    const inputs=structuredClone(tables);const output=sortRows(circuitOracle(family,inputs,domain));
    states.push({name,writes,mutation_sql:sql.join('\n'),inputs,input_hash:digest(inputText(inputs)),expected:{rows:output,checksum:digest(outputText(output))}});
  }
  const initial=[];
  for(let id=1;id<=rows;id++)initial.push({table:'a',id,row:valueRow(domain,[id,Math.floor((id-1)/fanout),id%7-3])});
  for(const table of ['b','c'])for(let id=1;id<=7;id++)initial.push({table,id,row:valueRow(domain,[id,id-4,table==='b'?id%3-1:id%4],true)});
  state('initial',initial);
  state('duplicate_support',[{table:'a',id:rows+1,row:[rows+1,...tables.a[0].slice(1)]}]);
  state('retract_one_support',[{table:'a',id:1,row:null}]);
  state('batch_key_value_move',Array.from({length:batch},(_,i)=>({table:'a',id:i+2,row:valueRow(domain,[i+2,0,-i%3])})));
  state('right_support_move',[{table:'b',id:1,row:valueRow(domain,[1,0,0],true)},{table:'b',id:2,row:valueRow(domain,[2,0,-2],true)}]);
  state('third_side_move',[{table:'c',id:1,row:valueRow(domain,[1,0,-3],true)}]);
  state('clear_roots',tables.b.map(r=>({table:'b',id:r[0],row:null})));
  state('clear_edges',tables.a.map(r=>({table:'a',id:r[0],row:null})));
  state('cycle_seed',[{table:'b',id:1,row:valueRow(domain,[1,1,1],true,cycleVariants)},...[ [1,1,2],[2,2,3],[3,3,2],[4,1,4],[5,4,3] ].map(row=>({table:'a',id:row[0],row:valueRow(domain,row,false,cycleVariants)}))]);
  state('diamond_path_retract',[{table:'a',id:2,row:null}]);
  state('root_retract',[{table:'b',id:1,row:null}]);
  state('root_restore',[{table:'b',id:1,row:valueRow(domain,[1,1,1],true,cycleVariants)}]);
  state('cycle_break',[{table:'a',id:3,row:null}]);
  return {circuit:family,query:circuits[family],rows,batch_size:batch,fanout,...(domain==='integers'?{}:{value_domain:domain,table_schema:valueDomains[domain]}),states};
}
