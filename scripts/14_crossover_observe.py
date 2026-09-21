"""Recover the pinned C lab, measure all three arms, then capture diagnostics."""
import argparse
import hashlib
import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = 'e2052d5ae'
PREFIX = 'v6/labs/exec_shootout/postgres_pglite_ivm/'
parser = argparse.ArgumentParser()
parser.add_argument('--sprefa', type=Path, default=ROOT.parent/'sprefa')
parser.add_argument('--out', type=Path)
parser.add_argument('--reps', type=int, default=3)
args = parser.parse_args()
if args.reps < 1:
    parser.error('--reps must be positive')
out = (args.out or ROOT/'bench/results'/f'crossover-observe-{time.time_ns()}').resolve()
out.mkdir(parents=True, exist_ok=False)
env = dict(os.environ, CARGO_TARGET_DIR=str(ROOT/'target'))

def run(command, **kwargs):
    return subprocess.run(command, check=True, cwd=ROOT, env=env, **kwargs)

files = ['43_sqlite_competitive/'+name for name in ['0a_state.h', '0ab_expressions.h', '0b_delta.h', '0bc_nonmonotone.h', '0c_batch.h', '1_native.c', '4_shootout.py']]
files += ['42_sqlite_native_take2/7_shootout.py']
hashes = {}
for relative in files:
    content = run(['git', '-C', str(args.sprefa), 'show', f'{SOURCE}:{PREFIX}{relative}'], capture_output=True).stdout
    path = out/'source'/relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(content)
    hashes[relative] = hashlib.sha256(content).hexdigest()
suffix = '.dylib' if sys.platform == 'darwin' else '.so'
historical = out/('competitive'+suffix)
include = os.environ.get('SQLITE3_INCLUDE_DIR')
if not include and sys.platform == 'darwin':
    include = run(['brew', '--prefix', 'sqlite'], capture_output=True, text=True).stdout.strip()+'/include'
run(['cc', '-O2', '-Wall', '-Wextra', '-Werror', '-fPIC', '-shared', *(['-I'+include] if include else []), str(out/'source/43_sqlite_competitive/1_native.c'), '-o', str(historical)])
extension = run(['bash', 'scripts/0_build.sh', 'release'], capture_output=True, text=True).stdout.strip()
run(['cargo', 'build', '--offline', '--locked', '--release', '--manifest-path', 'bench/Cargo.toml', '--bin', 'crossover-dd', '--bin', 'crossover-profile'])
patch = run(['git', 'diff', 'HEAD', '--', 'src', 'Cargo.toml', 'Cargo.lock', 'bench', 'vendor', 'scripts'], capture_output=True).stdout
(out/'current.patch').write_bytes(patch)
(out/'source.json').write_text(json.dumps({'historical_revision': SOURCE, 'sha256': hashes, 'command': sys.argv, 'time_unix': time.time(), 'current_revision': run(['git', 'rev-parse', 'HEAD'], capture_output=True, text=True).stdout.strip(), 'current_patch_sha256': hashlib.sha256(patch).hexdigest()}, indent=2)+'\n')
run([sys.executable, 'bench/crossover/28_run.py', '--extension', extension, '--dd-bin', str(ROOT/'target/release/crossover-dd'), '--historical-extension', str(historical), '--historical-adapter', str(out/'source/43_sqlite_competitive/4_shootout.py'), '--reps', str(args.reps), '--out', str(out/'timings')])
for arm in ['current', 'historical']:
    run([str(ROOT/'target/release/crossover-profile'), str(out/'timings/12000-1000-200.json'), str(out/arm), arm, *([str(historical)] if arm == 'historical' else [])])
print(out)
