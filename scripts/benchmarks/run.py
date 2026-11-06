#!/usr/bin/env python3
"""Opt-in release baseline. Only self-created private synthetic DBs, never a library path."""
import argparse
import csv
from datetime import datetime, timezone
import hashlib
import io
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]


def quantile(values, fraction):
    """Nearest rank: sorted[ceil(p*N)-1], without interpolation."""
    if not values or not 0 < fraction <= 1:
        raise ValueError('quantile needs nonempty samples and 0 < p <= 1')
    if any(isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v) or v < 0
           for v in values):
        raise ValueError('timings must be finite nonnegative numbers')
    return sorted(values)[math.ceil(fraction * len(values)) - 1]


def summary_row(metric, case, condition, page, batch, values, counts):
    return dict(metric=metric, case=case, condition=condition, page=page, batch=batch,
                samples=len(values), p50_ms=quantile(values, .5), p95_ms=quantile(values, .95),
                returned_min=min(counts), returned_max=max(counts))


def summarize(query, imported, query_samples, import_samples):
    if query['kind'] != 'query' or imported['kind'] != 'import':
        raise ValueError('incorrect instrument output kind')
    validations = {v['case']: v for v in query['validation']}
    expected_cases = {'list', 'metadata_keyword', 'metadata_no_hit', 'collection',
                      'tag_all', 'tag_any', 'combined'}
    if set(validations) != expected_cases:
        raise ValueError('missing query cases')
    for v in validations.values():
        if v['expected_hits'] != v['actual_hits'] or not v['full_metadata_equal']:
            raise ValueError('query hit or metadata mismatch')
    if validations['list']['actual_hits'] != query['records']:
        raise ValueError('list count mismatch')
    groups = {}
    for sample in query['samples']:
        key = (sample['case'], sample['condition'], sample['page'])
        groups.setdefault(key, []).append(sample)
    expected_keys = {
        (name, condition, page)
        for name, v in validations.items()
        for condition in ('reopened_connection_os_cache_uncontrolled', 'warm_connection')
        for page in range(2 if v['actual_hits'] > query['page_limit'] else 1)
    }
    if set(groups) != expected_keys:
        raise ValueError('query page/condition coverage mismatch')
    rows = []
    for (name, condition, page), samples in sorted(groups.items()):
        if sorted(s['sample'] for s in samples) != list(range(query_samples)):
            raise ValueError('query repeat coverage mismatch')
        expected_count = min(query['page_limit'],
                             max(0, validations[name]['actual_hits'] - page * query['page_limit']))
        if any(s['returned'] != expected_count or s['expected_returned'] != expected_count for s in samples):
            raise ValueError('query page count mismatch')
        rows.append(summary_row('query', name, condition, page, '',
                                [s['elapsed_ms'] for s in samples], [s['returned'] for s in samples]))
        rows.append(summary_row('connection_open', name, condition, page, '',
                                [s['open_ms'] for s in samples], [s['returned'] for s in samples]))
    runs = imported['runs']
    if sorted(r['sample'] for r in runs) != list(range(import_samples)):
        raise ValueError('import repeat coverage mismatch')
    count = imported['records']
    batch_count = math.ceil(count / 100)
    for run in runs:
        if (run['stored_count'] != count or not run['full_metadata_equal']
                or run['memberships_validated'] != count or len(run['batches']) != batch_count):
            raise ValueError('import persisted count/metadata mismatch')
        for i, b in enumerate(run['batches']):
            expected = min(100, count - i * 100)
            if (b['batch'] != i or b['start'] != i * 100 or b['rows'] != expected
                    or b['created'] != expected or b['before_count'] != i * 100
                    or b['after_count'] != i * 100 + expected or b['merged'] != 0
                    or b['skipped'] != 0 or b['preview_db_wal_unchanged'] is not True):
                raise ValueError('import batch/write validation mismatch')
    for metric in ('parse', 'total'):
        rows.append(summary_row(metric, 'import', 'fresh_empty_db_each_run', '', '',
                                [r[metric + '_ms'] for r in runs], [r['stored_count'] for r in runs]))
    for i in range(batch_count):
        for metric in ('preview', 'decision', 'commit'):
            batches = [r['batches'][i] for r in runs]
            rows.append(summary_row(metric, 'import', 'sequential_batches', '', i,
                                    [b[metric + '_ms'] for b in batches], [b['created'] for b in batches]))
    return rows


def csv_text(rows):
    out = io.StringIO(newline='')
    writer = csv.DictWriter(out, fieldnames=list(rows[0]), lineterminator='\n')
    writer.writeheader()
    writer.writerows(rows)
    return out.getvalue()


def capture(args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def harness_digest():
    files = sorted((ROOT / 'scripts/benchmarks').glob('*.rs'))
    files += [Path(__file__), ROOT / 'crates/storage/Cargo.toml', ROOT / 'crates/daemon/Cargo.toml']
    digest = hashlib.sha256()
    for path in files:
        digest.update(str(path.relative_to(ROOT)).encode() + b'\0' + path.read_bytes())
    return digest.hexdigest()


def build(package, example, out):
    args = ['cargo', 'build', '--release', '--locked', '-p', package, '--example', example,
            '--message-format=json']
    with (out / f'{example}-build.log').open('w') as log:
        result = subprocess.run(args, cwd=ROOT, stdout=subprocess.PIPE, stderr=log, text=True, check=True)
        log.write(result.stdout)
    artifacts = [v for line in result.stdout.splitlines()
                   if (v := json.loads(line)).get('reason') == 'compiler-artifact'
                   and v['target']['name'] == example and v.get('executable')]
    if len(artifacts) != 1:
        raise ValueError('expected one release example artifact')
    artifact = artifacts[0]
    if artifact['profile']['debug_assertions'] or artifact['profile']['opt_level'] != '3':
        raise ValueError('baseline requires the default optimized release profile')
    executable = artifact['executable']
    evidence = dict(profile=artifact['profile'], features=artifact['features'],
                    executable_sha256=hashlib.sha256(Path(executable).read_bytes()).hexdigest())
    return executable, args, evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--representative', action='store_true', help='opt in to 10k query / 1k import')
    parser.add_argument('--query-samples', type=int, default=21)
    parser.add_argument('--import-samples', type=int, default=3)
    parser.add_argument('--out', default=datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ'))
    args = parser.parse_args()
    if not 1 <= args.query_samples <= 100 or not 1 <= args.import_samples <= 100:
        parser.error('sample counts must be 1..100')
    # A name, never an arbitrary user path. Evidence lives only in this checkout's target.
    if not args.out or Path(args.out).name != args.out or args.out in ('.', '..'):
        parser.error('--out must be a single new directory name')
    os.umask(0o077)
    parent = ROOT / 'target/backend-benchmark'
    parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    if parent.is_symlink() or parent.resolve() != parent:
        parser.error('benchmark output parent must be a real dedicated target directory')
    out = parent / args.out
    out.mkdir(mode=0o700)  # refuse overwriting prior evidence
    query_count, import_count = (10000, 1000) if args.representative else (117, 7)
    environment = dict(
        started_utc=datetime.now(timezone.utc).isoformat(), machine=platform.machine(),
        model=capture(['sysctl', '-n', 'hw.model']), cpu=capture(['sysctl', '-n', 'machdep.cpu.brand_string']),
        physical_cpu_count=capture(['sysctl', '-n', 'hw.physicalcpu']),
        logical_cpu_count=capture(['sysctl', '-n', 'hw.logicalcpu']),
        ram_bytes=int(capture(['sysctl', '-n', 'hw.memsize'])), os=capture(['sw_vers']),
        rust=capture(['rustc', '-Vv']), cargo=capture(['cargo', '-V']), profile='release',
        build_flags='--release --locked; no feature flags; default Cargo release profile',
        build_environment={k: os.environ.get(k) for k in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET')},
        git_head=capture(['git', 'rev-parse', 'HEAD']), git_status=capture(['git', 'status', '--porcelain']),
        harness_sha256=harness_digest(), cargo_lock_sha256=hashlib.sha256((ROOT / 'Cargo.lock').read_bytes()).hexdigest(),
        query_records=query_count, import_records=import_count,
        query_samples=args.query_samples, import_samples=args.import_samples,
        limits='headless only; OS cache uncontrolled; no GUI, PDF, FTS, startup, total RSS or 16GB acceptance',
    )
    environment['artifacts'] = {}
    (out / 'environment.json').write_text(json.dumps(environment, indent=2) + '\n')
    commands = []
    reports = []
    # Keep measurements sequential: no concurrent query/import or workspace tests.
    for package, example, count, repeats, name in (
        ('cm-storage', 'backend_bench_query', query_count, args.query_samples, 'query'),
        ('cm-daemon', 'backend_bench_import', import_count, args.import_samples, 'import'),
    ):
        print(f'Building/running {name}: {count} records, {repeats} repeats', flush=True)
        executable, build_args, evidence = build(package, example, out)
        environment['artifacts'][example] = evidence
        (out / 'environment.json').write_text(json.dumps(environment, indent=2) + '\n')
        run_args = [executable, str(count), str(repeats)]
        with (out / f'{name}.json').open('w') as output, (out / f'{name}.log').open('w') as log:
            subprocess.run(run_args, cwd=ROOT, stdout=output, stderr=log, check=True)
        reports.append(json.loads((out / f'{name}.json').read_text()))
        commands.extend([build_args, [f'target/release/examples/{example}', str(count), str(repeats)]])
    rows = summarize(*reports, args.query_samples, args.import_samples)
    summary = dict(quantile='nearest rank ceil(p*N)-1; small N p95 may be maximum; no confidence interval',
                   environment_file='environment.json', query_file='query.json', import_file='import.json',
                   commands=commands, rows=rows)
    (out / 'summary.json').write_text(json.dumps(summary, indent=2) + '\n')
    (out / 'summary.csv').write_text(csv_text(rows))
    print(out)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f'benchmark failed: {error}', file=sys.stderr)
        sys.exit(1)
