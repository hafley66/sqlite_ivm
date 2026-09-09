// Native PostgreSQL and embedded PGlite share query transport and fixture logic.
import {createRequire} from 'node:module';
const require=createRequire(import.meta.url);
export async function openPostgres(embedded=false,directory){
  if(!embedded){const {Client}=require('pg');const client=new Client();await client.connect();return client;}
  const {PGlite}=await import('@electric-sql/pglite');
  const {pg_ivm}=await import('@electric-sql/pglite-pg_ivm');
  const db=new PGlite(directory,{extensions:{pg_ivm}});await db.waitReady;
  return {
    async query(sql,params){
      if(typeof sql==='object')return db.query(sql.text,sql.values??[],{rowMode:sql.rowMode});
      if(params)return db.query(sql,params);
      const results=await db.exec(sql);return results.at(-1)??{rows:[],fields:[]};
    },
    end:()=>db.close(),
  };
}
