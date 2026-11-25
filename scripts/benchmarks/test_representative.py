"""#30 representative benchmark: generator determinism/oracle and summary rules (no build)."""
import tempfile
import unittest
from pathlib import Path

from benchmarks import pdfgen
from benchmarks import representative as rep


class Generator(unittest.TestCase):
    def test_corpus_is_deterministic_and_oracle_matches_planting(self):
        with tempfile.TemporaryDirectory() as root:
            a = pdfgen.write_corpus(Path(root) / 'a', 30, 3, 40)
            b = pdfgen.write_corpus(Path(root) / 'b', 30, 3, 40)
            self.assertEqual(a, b)
            self.assertEqual(sorted(p.name for p in (Path(root) / 'a').iterdir()),
                             ['pdf-0000.pdf', 'pdf-0001.pdf', 'pdf-0002.pdf'])
            for name in ('pdf-0000.pdf', 'pdf-0002.pdf'):
                self.assertEqual((Path(root) / 'a' / name).read_bytes(), (Path(root) / 'b' / name).read_bytes())
        pages = 3 * 40
        # Planted-only terms: the background vocabularies never contain their characters.
        self.assertEqual([p[0] * 40 + p[1] for p in a['terms']['電池']['hits']],
                         [g for g in range(pages) if g % 7 == 0])
        self.assertEqual([p[0] * 40 + p[1] for p in a['terms']['lithium']['hits']],
                         [g for g in range(pages) if g % 5 == 0])
        single = {p[0] * 40 + p[1] for p in a['terms']['電']['hits']}
        union = {g for g in range(pages) if g % 7 == 0 or g % 13 == 0 or g % 97 == 0 or g % 11 == 0}
        self.assertEqual(single, union)
        self.assertEqual(a['terms']['固体電解質']['scalars'], 5)
        with self.assertRaises(FileExistsError):
            pdfgen.write_corpus(Path(__file__).parent, 30, 1, 1)

    def test_text_encodings_are_bounded(self):
        self.assertEqual(pdfgen.utf16_hex('電池'), '<96FB6C60>')
        with self.assertRaises(ValueError):
            pdfgen.utf16_hex('\U0001F600')
        with self.assertRaises(ValueError):
            pdfgen.latin('日本')
        self.assertEqual(pdfgen.latin('a(b)\\'), '(a\\(b\\)\\\\)')


class Summary(unittest.TestCase):
    def test_ps_cpu_time_formats(self):
        self.assertEqual(rep.Sampler.cpu_seconds('1-02:03:04'), 93784)
        self.assertEqual(rep.Sampler.cpu_seconds('00:01.50'), 1.5)
        self.assertEqual(rep.Sampler.cpu_seconds('0:00'), 0)

    def fake(self, gui):
        rows = []
        for case, ms, complete in [('collection_switch', 50.0, None), ('tag_and_switch', 10.0, None),
                                   ('metadata_keyword', 200.0, None), ('fts_bibliographic:needle', 5.0, True),
                                   ('fulltext_default:lithium', 20.0, True), ('fulltext_default:電', 1.0, False),
                                   ('fulltext_in_collection:電', 30.0, True)]:
            for sample in range(3):
                rows.append(dict(case=case, sample=sample, elapsed_ms=ms + sample, returned=1,
                                 detail={} if complete is None else dict(complete=complete)))
        queries = dict(rows=rows, recall=[], rusage=dict(max_rss_kib=2048))
        warm = dict(runs=[dict(first_page_ms=900.0, rss_kib_after_first_page=1024),
                          dict(first_page_ms=20.0, rss_kib_after_first_page=1024),
                          dict(first_page_ms=30.0, rss_kib_after_first_page=2048)])
        return rep.summarize(queries, warm, dict(peak_rss_kib=4096), {}, {}, gui)

    def test_targets_are_met_missed_or_unmeasured_never_assumed(self):
        summary = self.fake(None)
        status = {r['metric']: r['status'] for r in summary['targets']}
        self.assertEqual(status, {
            'warm_start_ms': 'met', 'collection_switch_p95_ms': 'met',
            'metadata_search_p95_ms': 'missed', 'fulltext_search_p95_ms': 'met',
            'gui_idle_rss_mib': 'unmeasured', 'pdf_view_rss_mib': 'unmeasured'})
        measured = summary['measured']
        # Repeat 0 (first start after indexing) is excluded from warm start.
        self.assertEqual(measured['warm_start_ms'], 20.0)
        # Incomplete bounded short-term answers are not counted as full-text answers.
        self.assertEqual(measured['fulltext_search_p95_ms'], 22.0)
        self.assertEqual(measured['fulltext_search_p95_ms_all_terms_default_scope'], 22.0)
        self.assertEqual(measured['daemon_idle_rss_mib_after_warm_start'], 2.0)
        gui = self.fake(dict(gui_idle_rss_mib=150.0, total_idle_rss_mib=170.0, pdf_view_rss_mib=400.0))
        status = {r['metric']: r['status'] for r in gui['targets']}
        self.assertEqual((status['gui_idle_rss_mib'], status['pdf_view_rss_mib']), ('met', 'missed'))


if __name__ == '__main__':
    unittest.main()
