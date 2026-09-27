import { copyFileSync, mkdirSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { from, toArray, lastValueFrom } from 'rxjs';
import { expect, test } from 'vitest';

type Term = { functor: string; args: number[]; types: ('Int' | 'Id')[]; text?: string };
type Case = {
  name: string;
  program: { rels: { id: number; cols: ('Int' | 'Id')[] }[] };
  frontiers: { changes: { rel: number; row: number[]; w: number }[] }[];
  expected: [number, number[], number][][];
  constructors: Record<string, Term>;
  texts: Record<string, string>;
};
type Change = { rel: number; row: number[]; w: number };
const dir = resolve(import.meta.dirname, '../../../target/ivm-rxjs-fixtures');
const generatedDir = resolve(import.meta.dirname, '.generated');
mkdirSync(generatedDir, { recursive: true });
const cases = JSON.parse(readFileSync(resolve(dir, 'cases.json'), 'utf8')) as Case[];

test('three op module declarations', () => {
  const source = readFileSync(resolve(dir, 'shape_3.ts'), 'utf8');
  const declarations = source.split('\n')
    .filter(line => /^  const n\d+: Observable<Batch> =/.test(line))
    .map(line => line.match(/const n\d+: Observable<Batch> = (?:defer\(\(\) => of\(0\)|n\d+|merge\(n\d+, n\d+\))/)![0]);
  expect(declarations).toMatchInlineSnapshot(`
    [
      "const n0: Observable<Batch> = defer(() => of(0)",
      "const n1: Observable<Batch> = n0",
      "const n2: Observable<Batch> = merge(n0, n1)",
    ]
  `);
});

function decode(cell: number, ty: 'Int' | 'Id', constructors: Record<string, Term>, texts: Record<string, string>): unknown {
  if (ty === 'Int') return cell;
  const term = constructors[cell];
  if (term) return [term.functor, ...term.args.map((v, i) => decode(v, term.types[i], constructors, texts))];
  if (texts[cell] !== undefined) return ['text', texts[cell]];
  return ['atom', cell];
}
function normalize(changes: Change[], testCase: Case, constructors: Record<string, Term>, texts: Record<string, string>) {
  return changes.map(({ rel, row, w }) => {
    const types = testCase.program.rels.find(r => r.id === rel)!.cols;
    return { rel, row: row.map((cell, i) => decode(cell, types[i], constructors, texts)), w };
  }).sort((a, b) => a.rel - b.rel || JSON.stringify(a.row).localeCompare(JSON.stringify(b.row)));
}

test.each(cases)('$name', async testCase => {
  copyFileSync(resolve(dir, `${testCase.name}.ts`), resolve(generatedDir, `${testCase.name}.ts`));
  const generated = await import(/* @vite-ignore */ `./.generated/${testCase.name}.ts`) as {
    run: (frontiers$: import('rxjs').Observable<Case['frontiers'][number]>) => import('rxjs').Observable<Change[]>;
    dictionary: symbol;
  };
  const actual = await lastValueFrom(generated.run(from(testCase.frontiers)).pipe(toArray()));
  expect(actual).toHaveLength(testCase.expected.length);
  for (let i = 0; i < actual.length; i++) {
    const batch = actual[i] as Change[] & { [key: symbol]: { byId: Record<string, Term> } };
    const terms = batch[generated.dictionary].byId;
    const tsConstructors = Object.fromEntries(Object.entries(terms).filter(([, term]) => term.text === undefined));
    const tsTexts = Object.fromEntries(Object.entries(terms).filter(([, term]) => term.text !== undefined).map(([id, term]) => [id, term.text!]));
    const expected = testCase.expected[i].map(([rel, row, w]) => ({ rel, row, w }));
    expect(normalize(batch, testCase, tsConstructors, tsTexts)).toEqual(normalize(expected, testCase, testCase.constructors, testCase.texts));
  }
});
