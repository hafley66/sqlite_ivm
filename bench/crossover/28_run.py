"""Run the recovered crossover fixture and consumers; keep every process receipt."""
import argparse
import hashlib
import json
import statistics
import subprocess
import sys
import time
from pathlib import Path

LAB = Path(__file__).resolve().parent
CASES = [(400, 10, 10), (12000, 10, 10), (400, 10, 200),
         (1200, 10, 200), (4000, 10, 200), (12000, 10, 200),
         (12000, 1000, 200), (12000, 1, 200), (12000, 100, 200)]
parser = argparse.ArgumentParser()
parser.add_argument('--extension', required=True, type=Path)
parser.add_argument('--dd-bin', required=True, type=Path)
parser.add_argument('--out', type=Path)
parser.add_argument('--reps', type=int, default=3)
parser.add_argument('--historical-extension', type=Path)
parser.add_argument('--historical-adapter', type=Path)
args = parser.parse_args()
if args.reps < 1:
    parser.error('--reps must be positive')
if bool(args.historical_extension) != bool(args.historical_adapter):
    parser.error('historical extension and adapter must be supplied together')
out = args.out or LAB.parent / 'results' / f'crossover-{time.time_ns()}'
out.mkdir(parents=True, exist_ok=False)
args.extension = args.extension.resolve()
args.dd_bin = args.dd_bin.resolve()
metadata = {
    'command': sys.argv,
    'time_unix': time.time(),
    'repetitions': args.reps,
    'warmups': 0,
    'source': 'sprefa@e2052d5ae',
    'sha256': {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in
               [args.extension, args.dd_bin, LAB/'9_crossover_workload.mjs', LAB/'22_crossover_dd.rs', LAB/'26_sqlite_plugin_adapter.py', LAB/'19_sqlite_template_adapter.py', Path(__file__), *([args.historical_extension, args.historical_adapter] if args.historical_extension else [])]},
}
(out/'0_run.json').write_text(json.dumps(metadata, indent=2)+'\n')
print('| rows | batch | fanout | sqlite-ivm ms | DD ms | IVM/DD |', flush=True)
print('|---:|---:|---:|---:|---:|---:|', flush=True)
summary = []
for rows, batch, fanout in CASES:
    name = f'{rows}-{batch}-{fanout}'
    fixture = out/f'{name}.json'
    generate = "import {makeCrossoverFixture} from './9_crossover_workload.mjs'; console.log(JSON.stringify(makeCrossoverFixture(...process.argv.slice(1).map(Number))));"
    with fixture.open('w') as handle:
        subprocess.run(['node', '--input-type=module', '-e', generate, str(rows), str(batch), str(fanout)], cwd=LAB, stdout=handle, check=True, timeout=30)
    expected = json.loads(fixture.read_text())['states']
    timings = {'sqlite-ivm': [], 'dd': []}
    if args.historical_extension:
        timings['historical-counted'] = []
    for rep in range(args.reps):
        arms = list(timings)
        arms = arms[rep % len(arms):] + arms[:rep % len(arms)]
        for arm in arms:
            prefix = out/f'{name}-{arm}-{rep}'
            command = ([str(args.dd_bin), str(fixture)] if arm == 'dd' else
                       [sys.executable, str(LAB/'26_sqlite_plugin_adapter.py'), '--extension', str(args.extension),
                        '--fixture', str(fixture), '--db', str(prefix)+'.db'])
            if arm == 'historical-counted':
                command = [sys.executable, str(args.historical_adapter), '--extension', str(args.historical_extension),
                           '--fixture', str(fixture), '--db', str(prefix)+'.db', '--source-views', '--lazy', '--counter-mode', 'counted']
            with Path(str(prefix)+'.jsonl').open('w') as stdout, Path(str(prefix)+'.stderr').open('w') as stderr:
                subprocess.run(command, stdout=stdout, stderr=stderr, check=True, timeout=120)
            records = [json.loads(line) for line in Path(str(prefix)+'.jsonl').read_text().splitlines()]
            mutations = [record for record in records if record.get('event') == 'mutation']
            assert [(r['state'], r['input_hash'], r['checksum']) for r in mutations] == [
                (state['name'], state['input_hash'], state['expected']['checksum']) for state in expected]
            totals = [record for record in records if record.get('event') == 'case-total']
            assert len(totals) == 1 and totals[0]['status'] == 'ok'
            timings[arm].append(totals[0]['update_plus_query_ms'])
    ivm, dd = [statistics.median(timings[arm]) for arm in ['sqlite-ivm', 'dd']]
    summary.append({'rows': rows, 'batch': batch, 'fanout': fanout, 'samples_ms': timings, 'ivm_over_dd': ivm/dd})
    (out/'report.json').write_text(json.dumps(summary, indent=2)+'\n')
    print(f'| {rows} | {batch} | {fanout} | {ivm:.3f} | {dd:.3f} | {ivm/dd:.2f} |', flush=True)
    if args.historical_extension:
        print(f'  historical counted: {statistics.median(timings["historical-counted"]):.3f} ms', flush=True)
print(out, flush=True)
