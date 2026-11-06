import copy
import csv
import io
import math
import unittest

from benchmarks.run import csv_text, quantile, summarize


def reports():
    names = ['list', 'metadata_keyword', 'metadata_no_hit', 'collection', 'tag_all', 'tag_any', 'combined']
    validation = [dict(case=name, expected_hits=3 if name == 'list' else 0,
                       actual_hits=3 if name == 'list' else 0, full_metadata_equal=True) for name in names]
    samples = [dict(case=name, condition=condition, page=page, sample=i,
                    elapsed_ms=i + 1, open_ms=0, returned=returned, expected_returned=returned)
               for name in names for condition in ('warm_connection', 'reopened_connection_os_cache_uncontrolled')
               for page, returned in ([(0, 2), (1, 1)] if name == 'list' else [(0, 0)]) for i in range(3)]
    query = dict(kind='query', records=3, page_limit=2, validation=validation, samples=samples)
    batches = [dict(batch=0, start=0, rows=3, created=3, before_count=0, after_count=3,
                    merged=0, skipped=0, preview_db_wal_unchanged=True,
                    preview_ms=2, decision_ms=.5, commit_ms=3)]
    imported = dict(kind='import', records=3, runs=[dict(sample=0, stored_count=3, full_metadata_equal=True,
                                                       memberships_validated=3, parse_ms=1, total_ms=9, batches=batches)])
    return query, imported


class InstrumentTests(unittest.TestCase):
    def test_nearest_rank_and_invalid_measurements(self):
        self.assertEqual(quantile([9, 1, 7, 3, 5], .5), 5)
        self.assertEqual(quantile(list(range(1, 22)), .95), 20)
        self.assertEqual(quantile([8], .95), 8)
        for values, fraction in [([], .5), ([1], 0), ([1], 1.1), ([math.nan], .5),
                                 ([math.inf], .5), ([-1], .5), ([True], .5)]:
            with self.assertRaises(ValueError):
                quantile(values, fraction)

    def test_summary_counts_quantiles_and_csv_round_trip(self):
        query, imported = reports()
        rows = summarize(query, imported, 3, 1)
        row = next(r for r in rows if r['metric'] == 'query' and r['case'] == 'list' and r['page'] == 1)
        self.assertEqual((row['p50_ms'], row['p95_ms'], row['returned_min'], row['returned_max']), (2, 3, 1, 1))
        parsed = list(csv.DictReader(io.StringIO(csv_text(rows))))
        self.assertNotIn('\r', csv_text(rows))
        self.assertEqual(len(parsed), len(rows))
        self.assertEqual(parsed[0]['metric'], rows[0]['metric'])

    def test_incomplete_samples_bad_counts_and_preview_writes_fail(self):
        query, imported = reports()
        bad_query = copy.deepcopy(query)
        bad_query['samples'].pop()
        bad_count = copy.deepcopy(query)
        bad_count['samples'][0]['returned'] += 1
        bad_import = copy.deepcopy(imported)
        bad_import['runs'][0]['batches'][0]['preview_db_wal_unchanged'] = False
        for q, i in [(bad_query, imported), (bad_count, imported), (query, bad_import)]:
            with self.assertRaises(ValueError):
                summarize(q, i, 3, 1)


if __name__ == '__main__':
    unittest.main()
