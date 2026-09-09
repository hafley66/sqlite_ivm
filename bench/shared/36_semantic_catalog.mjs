// Additional bounded-integer SQL contracts. Output ordering is normalized;
// top-k/window selection order is specified by unique input id as a tie-breaker.
import { makeCircuitFixture,sortRows,digest,outputText } from './30_circuit_workload.mjs';
export const semanticCircuits = {
  minmax: {columns:4,query:'SELECT k AS c0,COUNT(*) AS c1,MIN(v) AS c2,MAX(v) AS c3 FROM a GROUP BY k'},
  count_distinct: {columns:2,query:'SELECT k AS c0,COUNT(DISTINCT v) AS c1 FROM a GROUP BY k'},
  union_set: {columns:2,query:'SELECT k AS c0,v AS c1 FROM a UNION SELECT k AS c0,v AS c1 FROM b'},
  except_set: {columns:2,query:'SELECT k AS c0,v AS c1 FROM a EXCEPT SELECT k AS c0,v AS c1 FROM b'},
  intersect_set: {columns:2,query:'SELECT k AS c0,v AS c1 FROM a INTERSECT SELECT k AS c0,v AS c1 FROM b'},
  topk: {columns:2,query:'SELECT k AS c0,v AS c1 FROM a ORDER BY v DESC,id LIMIT 3'},
  window_rank: {columns:3,query:'SELECT k AS c0,v AS c1,ROW_NUMBER() OVER (PARTITION BY k ORDER BY v,id) AS c2 FROM a'},
  subquery: {columns:2,query:'SELECT k AS c0,v AS c1 FROM (SELECT k,v FROM a WHERE v>=0) q'},
  cte: {columns:2,query:'WITH q AS (SELECT k,v FROM a WHERE v>=0) SELECT k AS c0,v AS c1 FROM q'},
};
export function semanticOracle(family,{a,b}) {
  const project=rows=>rows.map(([,k,v])=>[k,v]);
  const unique=rows=>[...new Map(rows.map(r=>[JSON.stringify(r),r])).values()];
  const groups=new Map();for(const row of a){const g=groups.get(row[1])??[];g.push(row);groups.set(row[1],g);}
  switch(family){
    case 'minmax': return [...groups].map(([k,rows])=>[k,rows.length,Math.min(...rows.map(r=>r[2])),Math.max(...rows.map(r=>r[2]))]);
    case 'count_distinct': return [...groups].map(([k,rows])=>[k,new Set(rows.map(r=>r[2])).size]);
    case 'union_set': return unique(project([...a,...b]));
    case 'except_set': return unique(project(a)).filter(([k,v])=>!b.some(r=>r[1]===k&&r[2]===v));
    case 'intersect_set': return unique(project(a)).filter(([k,v])=>b.some(r=>r[1]===k&&r[2]===v));
    case 'topk': return project(a.toSorted((x,y)=>y[2]-x[2]||x[0]-y[0]).slice(0,3));
    case 'window_rank': return [...groups].flatMap(([k,rows])=>rows.toSorted((x,y)=>x[2]-y[2]||x[0]-y[0]).map((r,i)=>[k,r[2],i+1]));
    case 'subquery': case 'cte': return project(a.filter(r=>r[2]>=0));
    default: throw new Error(`unknown semantic circuit ${family}`);
  }
}
export function makeSemanticFixture(family,rows=24,batch=3,fanout=4){
  const specification=semanticCircuits[family];if(!specification)throw new Error('unknown semantic circuit');
  const fixture=makeCircuitFixture('pipeline',rows,batch,fanout);
  fixture.circuit=family;fixture.query=specification.query;fixture.columns=specification.columns;
  for(const state of fixture.states){const result=sortRows(semanticOracle(family,state.inputs));state.expected={rows:result,checksum:digest(outputText(result))};}
  return fixture;
}
