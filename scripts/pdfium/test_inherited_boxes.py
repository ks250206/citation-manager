#!/usr/bin/env python3
"""Real PDF-tree checks; run in the pinned PDF tools lane, not the dependency-free lane."""
from pathlib import Path
import tempfile
import unittest
from pypdf import PdfReader, PdfWriter
from pypdf.generic import NumberObject
from inherited_boxes import inspect_structure

FIXTURE = Path(__file__).resolve().parents[2]/'fixtures/pdf/inherited.pdf'

class InheritedBoxes(unittest.TestCase):
    def test_committed_raw_tree(self):
        self.assertEqual([p['direct_entries'] for p in inspect_structure(FIXTURE)['pages']], [[], ['/MediaBox']])

    def test_rejects_silently_flattened_inheritance(self):
        reader = PdfReader(FIXTURE)
        writer = PdfWriter(clone_from=reader)
        list(writer.pages)  # This propagates inherited entries into page dictionaries.
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/'flattened.pdf'
            writer.write(path)
            with self.assertRaises(AssertionError):
                inspect_structure(path)

    def test_rejects_changed_parent_box(self):
        writer = PdfWriter(clone_from=PdfReader(FIXTURE))
        writer._pages.get_object()['/CropBox'][0] = NumberObject(21)
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)/'wrong-parent.pdf'
            writer.write(path)
            with self.assertRaises(AssertionError):
                inspect_structure(path)

if __name__ == '__main__':
    unittest.main(verbosity=2)
