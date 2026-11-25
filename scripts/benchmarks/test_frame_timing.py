"""#86 frame-timing harness: GPUI log parsing, pairing and per-phase summary (no build, no GUI)."""
import unittest

from benchmarks import frame_timing as ft


def span(kind, start_us, end_us, completed=True, page=None):
    return dict(kind=kind, start_us=start_us, end_us=end_us,
                duration_us=None if end_us is None else end_us - start_us,
                first_frame=None, last_frame=None, page=page, completed=completed)


class Summary(unittest.TestCase):
    def test_parses_rust_debug_durations_only(self):
        lines = ['frame duration: 1.5ms\n', 'frame duration: 850µs', 'other: 3ms',
                 'frame duration: 20.25ms', 'frame duration: 900ns', 'frame duration: 1.2s']
        values = [ft.parse_gpui_frame(line) for line in lines]
        self.assertEqual(values[:2] + values[3:], [1.5, 0.85, 20.25, 900e-6, 1200.0])
        self.assertIsNone(values[2])

    def test_stats_counts_budget_overruns(self):
        row = ft.stats([5.0, 10.0, 20.0, 1.0])
        self.assertEqual((row['n'], row['p50_ms'], row['p95_ms'], row['max_ms']), (4, 5.0, 20.0, 20.0))
        self.assertEqual((row['over_60hz'], row['over_120hz']), (1, 2))
        self.assertIsNone(ft.stats([]))

    def test_phases_attribute_frames_and_spans_by_unix_time(self):
        origin = 1_000_000_000  # 1000 s after the epoch, in µs
        doc = dict(origin_unix_us=origin, frame_count=4, dropped_frames=0,
                   frames=[[0, 4_000], [1_000_000, 20_000], [1_100_000, 6_000], [5_000_000, 1_000]],
                   spans=[span('page_turn', 900_000, 1_120_000, page=2),
                          span('page_turn', 1_050_000, 1_060_000, completed=False),
                          span('pdf_first_page', 4_000_000, 5_001_000)],
                   open_spans=[])
        phases = {'turns': dict(start_unix=1000.8, end_unix=1001.5, expect='page_turn'),
                  'open': dict(start_unix=1003.5, end_unix=1005.5, expect='pdf_first_page', input_unix=1003.6),
                  'empty': dict(start_unix=1010.0, end_unix=1011.0, expect='list_scroll')}
        gpui = [[1000.0, 9.0], [1001.03, 30.0], [1001.11, 7.0], [1005.0, 2.0], [1009.0, 2.0]]
        summary = ft.summarize(doc, gpui, phases)
        self.assertEqual(summary['gpui_logged_frames'], 5)
        turns = summary['phases']['turns']
        self.assertEqual(turns['app_frame']['n'], 2)
        self.assertEqual(turns['app_frame']['max_ms'], 20.0)
        self.assertEqual(turns['gpui_draw_present']['max_ms'], 30.0)
        self.assertEqual(turns['spans']['page_turn']['completed'], 1)
        self.assertEqual(turns['spans']['page_turn']['incomplete'], 1)
        self.assertEqual(turns['spans']['page_turn']['duration']['max_ms'], 220.0)
        self.assertNotIn('over_60hz', turns['spans']['page_turn']['duration'])
        self.assertTrue(turns['observed'])
        opened = summary['phases']['open']
        self.assertAlmostEqual(opened['input_to_first_page_ms'], 1401.0, places=3)
        self.assertFalse(summary['phases']['empty']['observed'])
        self.assertIsNone(summary['phases']['empty']['app_frame'])
        self.assertEqual(summary['all_frames']['app_frame']['n'], 4)


class Points(unittest.TestCase):
    def test_negative_coordinates_count_from_the_far_edge(self):
        self.assertEqual(ft.resolve((10, -63), (40, 20), (1320, 860)), (50, 817))
        self.assertEqual(ft.resolve((-436, 5), (0, 53), (1024, 680)), (588, 58))
        self.assertEqual(ft.resolve((0.5, 147), (0, 53), (1024, 680)), (512.0, 200))

    def test_macos_key_table_covers_every_typed_character(self):
        typed = 'Open paper:// Link' + ''.join(str(p) for p in ft.JUMPS) + 'jkJKq'
        for char in typed:
            self.assertIn(ft.Quartz.SHIFTED.get(char, char.lower()), ft.Quartz.CODES, char)
        self.assertEqual((ft.Quartz.CODES['j'], ft.Quartz.CODES['5'], ft.Quartz.CODES[' ']), (38, 23, 49))


if __name__ == '__main__':
    unittest.main()
