import {readFileSync,writeFileSync,mkdtempSync,rmSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
const [binary,extension]=process.argv.slice(2);
if(!binary||!extension)throw new Error('native consumer and extension paths required');
const fixtures=JSON.parse(readFileSync(new URL('../tests/fixtures/0_shared.json',import.meta.url),'utf8'));
const root=mkdtempSync(join(tmpdir(),'sqlite-ivm-native-'));
try {
  for(const fixture of fixtures){
    const input=join(root,`${fixture.circuit}.json`);
    writeFileSync(input,JSON.stringify(fixture));
    const records=execFileSync(binary,['--fixture',input,'--db',join(root,`${fixture.circuit}.sqlite`),'--extension',extension],{encoding:'utf8',timeout:120000}).trim().split('\n').map(JSON.parse);
    if(records.some(r=>r.status!=='ok')||records.filter(r=>r.event==='mutation'&&r.exact_input_output_validated).length!==fixture.states.length||records.at(-1).event!=='case-total')throw new Error(`incomplete receipt: ${fixture.circuit}`);
    console.log(`PASS: native ${fixture.circuit}, ${fixture.states.length} states, reopen, rename, rollback`);
  }
} finally {rmSync(root,{recursive:true,force:true});}
