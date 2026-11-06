#!/usr/bin/env python3
"""#53 opt-in pinned SQLite experiment. Fresh private TempDB only; output refuses overwrite."""
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
import re
import stat
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[2]
HERE = Path(__file__).resolve().parent
SQLITE_BUILD_OVERRIDES = ('LIBSQLITE3_SYS_USE_PKG_CONFIG', 'SQLITE3_LIB_DIR',
                          'SQLITE3_INCLUDE_DIR', 'SQLITE3_STATIC', 'LIBSQLITE3_FLAGS',
                          'SQLITE_MAX_VARIABLE_NUMBER', 'SQLITE_MAX_EXPR_DEPTH', 'SQLITE_MAX_COLUMN')


def quantile(values, fraction):
    if not values or not 0 < fraction <= 1:
        raise ValueError('nonempty samples and 0 < p <= 1 required')
    if any(isinstance(v, bool) or not isinstance(v, (int, float)) or not math.isfinite(v) or v < 0
           for v in values):
        raise ValueError('timings must be finite nonnegative numbers')
    return sorted(values)[math.ceil(fraction * len(values)) - 1]


def summarize(raw, fixture, oracle, repetitions, warmups):
    """Fail closed on missing/duplicate cases, wrong hitsets, bounds or summary values."""
    if (raw['kind'] != 'search_tokenizer_experiment' or raw['documents'] != len(fixture['documents'])
            or raw['repetitions_per_case'] != repetitions or raw['warmups_per_case'] != warmups
            or raw['fts5_enabled'] is not True or 'ENABLE_FTS5' not in raw['compile_options']):
        raise ValueError('wrong instrument, engine or repeat coverage')
    documents = {d['id']: d for d in fixture['documents']}
    if len(documents) != len(fixture['documents']):
        raise ValueError('duplicate document ID')
    queries = {q['name']: q for q in oracle['queries']}
    fallback = {q['name']: q for q in oracle['fallback']}
    expected_keys = {('fts', t, name) for t in ('unicode61', 'trigram') for name in queries}
    expected_keys |= {('bounded_literal', '', name) for name in fallback}
    seen, vocab_seen, rows = set(), set(), []
    for observed in raw['observations']:
        series = observed['series']
        if series == 'vocabulary':
            tokenizer = observed['tokenizer']
            if tokenizer not in ('unicode61', 'trigram') or tokenizer in vocab_seen or not observed['tokens']:
                raise ValueError('missing or duplicate token observation')
            vocab_seen.add(tokenizer)
            continue
        tokenizer = observed.get('tokenizer', '')
        name = observed['case']
        key = (series, tokenizer, name)
        if key not in expected_keys or key in seen:
            raise ValueError('unexpected or duplicate series/case')
        seen.add(key)
        timing = observed['timing']
        samples = timing['samples_ms']
        if len(samples) != repetitions:
            raise ValueError('missing repetitions')
        p50, p95 = quantile(samples, .5), quantile(samples, .95)
        if (timing['p50_ms'], timing['p95_ms']) != (p50, p95):
            raise ValueError('instrument and summary quantiles disagree')
        if series == 'fts':
            expected = queries[name]
            hits = observed['hits']
            if (observed['text'] != expected['text'] or hits != expected[tokenizer]
                    or observed['expected_semantic_hits'] != hits
                    or observed['literal_expected'] != expected['literal']
                    or not set(hits) <= set(observed['candidate_ids'])):
                raise ValueError('FTS semantic hitset or candidate mismatch')
            wanted = expected['literal']
            recall = len(set(hits) & set(wanted)) / len(wanted) if wanted else None
            if observed['literal_recall'] != recall:
                raise ValueError('FTS literal recall mismatch')
            candidates = len(observed['candidate_ids'])
            scope = 'whole_synthetic_corpus'
            unsearched = 0
            cap = ''
        else:
            expected = fallback[name]
            if (observed['text'] != expected['text'] or observed['filter'] != {'bucket_eq': expected['bucket']}
                    or observed['candidate_cap'] != expected['cap']):
                raise ValueError('fallback filter/cap mismatch')
            eligible = sorted(i for i, d in documents.items() if d['bucket'] == expected['bucket'])
            selected = eligible[:expected['cap']]
            not_searched = eligible[expected['cap']:]
            excluded = sorted(set(documents) - set(eligible))
            result = observed['result']
            hits = result['hits']
            wanted = expected['expected_filtered']
            recall = len(set(hits) & set(wanted)) / len(wanted) if wanted else None
            if (result['eligible_count'] != len(eligible) or result['candidate_ids'] != selected
                    or len(selected) > expected['cap'] or not 1 <= expected['cap'] <= 64
                    or result['compared_bytes'] != sum(len(documents[i]['text'].encode()) for i in selected)
                    or hits != expected['expected_searched'] or not set(hits) <= set(selected)
                    or observed['fixture_oracle_searched_hits'] != hits
                    or observed['fixture_oracle_filtered_hits'] != wanted
                    or observed['fixture_oracle_recall_within_filter'] != recall
                    or observed['unsearched_eligible_ids'] != not_searched
                    or observed['excluded_by_filter_ids'] != excluded
                    or observed['truncated'] != bool(not_searched)
                    or observed['status'] != ('partial_unsearched' if not_searched else 'complete_within_filter')):
                raise ValueError('bounded scope/hitset/recall mismatch')
            candidates, scope, unsearched, cap = len(selected), observed['status'], len(not_searched), expected['cap']
        rows.append(dict(series=series, tokenizer=tokenizer, case=name, query=observed['text'],
                         samples=len(samples), p50_ms=p50, p95_ms=p95, candidate_count=candidates,
                         hits=len(hits), literal_oracle_hits=len(wanted), literal_oracle_recall=recall,
                         candidate_cap=cap, scope_status=scope, unsearched_within_filter=unsearched))
    if seen != expected_keys or vocab_seen != {'unicode61', 'trigram'}:
        raise ValueError('incomplete observation coverage')
    return rows


def csv_text(rows):
    out = io.StringIO(newline='')
    writer = csv.DictWriter(out, fieldnames=list(rows[0]), lineterminator='\n')
    writer.writeheader()
    writer.writerows(rows)
    return out.getvalue()


def capture(args):
    return subprocess.check_output(args, cwd=ROOT, text=True).strip()


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def new_output(name):
    if not name or Path(name).name != name or name in ('.', '..'):
        raise ValueError('--out must be a new single directory name')
    parent = ROOT / 'target/search-experiments'
    # Inspect both output ancestors before mkdir can follow either symlink.
    for path in (ROOT / 'target', parent):
        try:
            info = path.lstat()
        except FileNotFoundError:
            continue
        if not stat.S_ISDIR(info.st_mode) or path.resolve() != path:
            raise ValueError('dedicated target output parent must not contain symlinks')
    parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    if parent.is_symlink() or parent.resolve() != parent:
        raise ValueError('dedicated target output parent must not contain symlinks')
    out = parent / name
    out.mkdir(mode=0o700)  # exist_ok=False: never overwrite evidence
    return out


def reject_engine_overrides():
    if any(k in os.environ for k in SQLITE_BUILD_OVERRIDES):
        raise ValueError('remove SQLite engine build overrides; pinned bundled build required')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repetitions', type=int, default=31)
    parser.add_argument('--warmups', type=int, default=5)
    parser.add_argument('--out', default=datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ'))
    args = parser.parse_args()
    if not 1 <= args.repetitions <= 100 or not 1 <= args.warmups <= 20:
        parser.error('repetitions 1..100; warmups 1..20')
    os.umask(0o077)
    reject_engine_overrides()
    out = new_output(args.out)
    metadata = json.loads(capture(['cargo', 'metadata', '--locked', '--format-version', '1']))
    packages = {p['name']: p for p in metadata['packages'] if p['name'] in ('rusqlite', 'libsqlite3-sys')}
    pinned = {}
    for name, relative in [('rusqlite', 'src/lib.rs'), ('libsqlite3-sys', 'sqlite3/sqlite3.c')]:
        p = packages[name]
        source = Path(p['manifest_path']).parent
        pinned[name] = dict(version=p['version'], source=p['source'],
                            files={f: sha(source / f) for f in ('Cargo.toml', relative, *(['build.rs'] if name == 'libsqlite3-sys' else []))})
    sqlite_source = (Path(packages['libsqlite3-sys']['manifest_path']).parent / 'sqlite3/sqlite3.c').read_text()
    source_id = re.search(r'^#define SQLITE_SOURCE_ID\s+"([^"]+)"', sqlite_source, re.M).group(1)
    source_version = re.search(r'^#define SQLITE_VERSION\s+"([^"]+)"', sqlite_source, re.M).group(1)
    files = sorted(p for p in HERE.iterdir() if p.suffix in ('.rs', '.json', '.py'))
    files += [ROOT / 'Cargo.toml', ROOT / 'Cargo.lock', ROOT / 'crates/storage/Cargo.toml']
    env = dict(started_utc=datetime.now(timezone.utc).isoformat(), git_head=capture(['git', 'rev-parse', 'HEAD']),
               git_status=capture(['git', 'status', '--porcelain']), os=platform.platform(), machine=platform.machine(),
               rust=capture(['rustc', '-Vv']), cargo=capture(['cargo', '-V']), python=sys.version,
               pinned_engine_sources=pinned, source_sha256={str(p.relative_to(ROOT)): sha(p) for p in files},
               build_environment={k: os.environ.get(k) for k in (*SQLITE_BUILD_OVERRIDES, 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_TARGET', 'CARGO_TARGET_DIR', 'CC', 'CFLAGS', 'CPPFLAGS')},
               limitations='Small direct-SQL synthetic tokenizer experiment, not import/extraction/service/GUI/16GB/RSS or #18/#30 acceptance')
    if platform.system() == 'Darwin':
        env['hardware'] = {k: capture(['sysctl', '-n', k]) for k in
                           ('hw.model', 'machdep.cpu.brand_string', 'hw.memsize', 'hw.physicalcpu', 'hw.logicalcpu')}
        env['macos'] = capture(['sw_vers'])
    # libsqlite3-sys supports environment overrides that can bypass the pinned bundled build.
    reject_engine_overrides()
    build_args = ['cargo', 'build', '--release', '--locked', '-p', 'cm-storage', '--example', 'search_experiment', '--message-format=json']
    with (out / 'build.log').open('w') as log:
        build = subprocess.run(build_args, cwd=ROOT, text=True, stdout=subprocess.PIPE, stderr=log, check=True)
        log.write(build.stdout)
    artifacts = [v for line in build.stdout.splitlines() if (v := json.loads(line)).get('reason') == 'compiler-artifact'
                 and v['target']['name'] == 'search_experiment' and v.get('executable')]
    if len(artifacts) != 1:
        raise ValueError('expected one example artifact')
    a = artifacts[0]
    engine_artifacts = [v for line in build.stdout.splitlines() if (v := json.loads(line)).get('reason') == 'compiler-artifact'
                        and v['target']['name'] in ('rusqlite', 'libsqlite3_sys') and 'lib' in v['target']['kind']]
    engine_features = {v['target']['name']: v['features'] for v in engine_artifacts}
    if (set(engine_features) != {'rusqlite', 'libsqlite3_sys'}
            or any('bundled' not in f or set(f) & {'sqlcipher', 'loadable_extension', 'in_gecko'}
                   for f in engine_features.values())):
        raise ValueError('expected bundled rusqlite/libsqlite3-sys artifacts without alternate engines')
    env['engine_features'] = engine_features
    env['pinned_sqlite_source_id'] = source_id
    if a['profile']['debug_assertions'] or a['profile']['opt_level'] != '3':
        raise ValueError('default optimized release profile required')
    executable = a['executable']
    env['artifact'] = dict(executable=executable, sha256=sha(Path(executable)), profile=a['profile'], features=a['features'])
    run_args = [executable, str(args.repetitions), str(args.warmups)]
    env['commands'] = [build_args, run_args]
    env['environment_sha256'] = hashlib.sha256(json.dumps(env, sort_keys=True).encode()).hexdigest()
    (out / 'environment.json').write_text(json.dumps(env, indent=2) + '\n')
    print(f'Running pinned SQLite: {args.repetitions} samples after {args.warmups} warmups per case', flush=True)
    with (out / 'raw.json').open('w') as output, (out / 'run.log').open('w') as log:
        subprocess.run(run_args, cwd=ROOT, stdout=output, stderr=log, check=True)
    raw = json.loads((out / 'raw.json').read_text())
    if raw['sqlite_source_id'] != source_id or raw['sqlite_version'] != source_version:
        raise ValueError('runtime SQLite differs from pinned bundled C source')
    if raw['fixture_sha256'] != sha(HERE / 'fixture.json') or raw['expected_sha256'] != sha(HERE / 'expected.json'):
        raise ValueError('compiled fixtures differ from source')
    rows = summarize(raw, json.loads((HERE / 'fixture.json').read_text()),
                     json.loads((HERE / 'expected.json').read_text()), args.repetitions, args.warmups)
    summary = dict(environment_file='environment.json', raw_file='raw.json', raw_sha256=sha(out / 'raw.json'),
                   quantile=raw['quantile'], rows=rows)
    (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    (out / 'summary.csv').write_text(csv_text(rows))
    print(out)


if __name__ == '__main__':
    try:
        main()
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        print(f'experiment failed: {error}', file=sys.stderr)
        sys.exit(1)
