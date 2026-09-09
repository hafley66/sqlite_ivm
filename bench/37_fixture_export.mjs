// Shared shootout fixtures copied without semantic changes from sqlite-ivm-astra.
import {circuits,makeCircuitFixture} from './shared/30_circuit_workload.mjs';
import {semanticCircuits,makeSemanticFixture} from './shared/36_semantic_catalog.mjs';
import {writeFileSync} from 'node:fs';
const fixtures=[...Object.keys(circuits).map(n=>makeCircuitFixture(n)),...Object.keys(semanticCircuits).map(n=>makeSemanticFixture(n))];
writeFileSync(new URL('../tests/fixtures/0_shared.json',import.meta.url),JSON.stringify(fixtures,null,2)+'\n');
