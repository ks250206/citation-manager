import copy
import csv
import importlib.util
import io
import math
import os
from pathlib import Path
import unittest
from unittest.mock import patch
import tempfile

_spec = importlib.util.spec_from_file_location('search_experiment_runner', Path(__file__).with_name('run.py'))
runner = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(runner)


def reports():
    fixture = {'documents': [{'id': 1, 'bucket': 'b', 'text': 'abc'},
                             {'id': 2, 'bucket': 'b', 'text': 'x'}]}
    oracle = {'queries': [{'name': 'abc', 'text': 'abc', 'literal': [1], 'unicode61': [1], 'trigram': [1]}],
              'fallback': [{'name': 'partial', 'text': 'x', 'bucket': 'b', 'cap': 1,
                            'expected_filtered': [2], 'expected_searched': []}]}
    timing = {'samples_ms': [3, 1, 2], 'p50_ms': 2, 'p95_ms': 3}
    observations = []
    for tokenizer in ('unicode61', 'trigram'):
        observations.extend([
            dict(series='fts', tokenizer=tokenizer, case='abc', text='abc', hits=[1],
                 expected_semantic_hits=[1], literal_expected=[1], literal_recall=1,
                 candidate_ids=[1], timing=copy.deepcopy(timing)),
            dict(series='vocabulary', tokenizer=tokenizer, tokens=[{'term': 'abc', 'doc': 1}])])
    observations.append(dict(series='bounded_literal', case='partial', text='x', filter={'bucket_eq': 'b'},
                             candidate_cap=1, result=dict(eligible_count=2, candidate_ids=[1], hits=[], compared_bytes=3),
                             fixture_oracle_searched_hits=[], fixture_oracle_filtered_hits=[2],
                             fixture_oracle_recall_within_filter=0, unsearched_eligible_ids=[2],
                             excluded_by_filter_ids=[], truncated=True, status='partial_unsearched', timing=timing))
    return dict(kind='search_tokenizer_experiment', documents=2, repetitions_per_case=3,
                warmups_per_case=1, fts5_enabled=True, compile_options=['ENABLE_FTS5'],
                observations=observations), fixture, oracle


class SearchSummaryTests(unittest.TestCase):
    def test_quantiles_csv_and_zero_recall_still_partial(self):
        raw, fixture, oracle = reports()
        rows = runner.summarize(raw, fixture, oracle, 3, 1)
        self.assertEqual(len(rows), 3)
        self.assertEqual((rows[-1]['p50_ms'], rows[-1]['p95_ms']), (2, 3))
        self.assertEqual(rows[-1]['literal_oracle_recall'], 0)
        self.assertEqual(rows[-1]['scope_status'], 'partial_unsearched')
        parsed = list(csv.DictReader(io.StringIO(runner.csv_text(rows))))
        self.assertEqual(parsed[-1]['unsearched_within_filter'], '1')
        self.assertEqual(runner.quantile(list(range(1, 22)), .95), 20)
        for values in ([], [math.nan], [math.inf], [-1], [True]):
            with self.assertRaises(ValueError):
                runner.quantile(values, .95)

    def test_corrupted_hitsets_bounds_scope_coverage_and_summary_fail(self):
        raw, fixture, oracle = reports()
        changes = [
            lambda r: r['observations'].pop(),
            lambda r: r['observations'].append(copy.deepcopy(r['observations'][0])),
            lambda r: r['observations'][0].update(hits=[]),
            lambda r: r['observations'][0]['timing'].update(p95_ms=2.5),
            lambda r: r['observations'][0]['timing']['samples_ms'].pop(),
            lambda r: r['observations'][-1].update(status='complete_within_filter'),
            lambda r: r['observations'][-1].update(unsearched_eligible_ids=[]),
            lambda r: r['observations'][-1]['result'].update(candidate_ids=[1, 2]),
            lambda r: r['observations'][-1].update(fixture_oracle_recall_within_filter=1),
        ]
        for change in changes:
            bad = copy.deepcopy(raw)
            change(bad)
            with self.assertRaises(ValueError):
                runner.summarize(bad, fixture, oracle, 3, 1)

    def test_output_rejects_paths_and_existing_evidence(self):
        for value in ('', '.', '..', '/tmp/db', '../db', 'two/names'):
            with self.assertRaises(ValueError):
                runner.new_output(value)
        with tempfile.TemporaryDirectory() as temp, patch.object(runner, 'ROOT', Path(temp).resolve()):
            created = runner.new_output('evidence')
            marker = created / 'untouched'
            marker.write_text('preserve')
            with self.assertRaises(FileExistsError):
                runner.new_output('evidence')
            self.assertEqual(marker.read_text(), 'preserve')

    def test_symlink_ancestors_are_rejected_before_any_external_mkdir(self):
        for symlink_target in ('target', 'parent'):
            with tempfile.TemporaryDirectory() as temp:
                root = Path(temp).resolve() / 'root'
                outside = Path(temp).resolve() / 'outside'
                root.mkdir()
                outside.mkdir()
                if symlink_target == 'target':
                    (root / 'target').symlink_to(outside, target_is_directory=True)
                else:
                    (root / 'target').mkdir()
                    (root / 'target/search-experiments').symlink_to(outside, target_is_directory=True)
                with patch.object(runner, 'ROOT', root), self.assertRaises(ValueError):
                    runner.new_output('evidence')
                self.assertEqual(list(outside.iterdir()), [])

    def test_all_sqlite_engine_overrides_are_rejected(self):
        with patch.dict(os.environ, {}, clear=True):
            runner.reject_engine_overrides()
            for name in runner.SQLITE_BUILD_OVERRIDES:
                with patch.dict(os.environ, {name: 'override'}), self.assertRaises(ValueError):
                    runner.reject_engine_overrides()


if __name__ == '__main__':
    unittest.main()
