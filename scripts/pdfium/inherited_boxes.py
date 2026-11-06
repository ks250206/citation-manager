#!/usr/bin/env python3
"""Check unflattened page-tree dictionaries, before any PdfReader.pages access."""
import hashlib
from pypdf import PdfReader

if not __debug__:
    raise RuntimeError("verification requires assertions enabled; do not run Python with -O")


def inspect_structure(path):
    reader = PdfReader(path)
    tree = reader.trailer['/Root']['/Pages']
    assert tree['/Type'] == '/Pages' and tree['/Count'] == 2
    assert len(tree['/Kids']) == 2
    assert list(tree['/MediaBox']) == [0, 0, 400, 500]
    assert list(tree['/CropBox']) == [20, 30, 380, 470]
    assert tree['/Rotate'] == 90
    entries = []
    for i, ref in enumerate(tree['/Kids']):
        page = ref.get_object()
        assert page['/Type'] == '/Page'
        assert page.raw_get('/Parent') == tree.indirect_reference
        keys = [k for k in ['/MediaBox', '/CropBox', '/Rotate'] if k in page]
        assert keys == ([] if i == 0 else ['/MediaBox']), (i, keys)
        if i == 1:
            assert list(page['/MediaBox']) == [0, 0, 400, 500]
        entries.append({'page_index': i, 'direct_entries': keys})
    return {'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
            'method': 'raw /Root/Pages/Kids, without PdfReader.pages flattening',
            'parent_media': [0, 0, 400, 500], 'parent_crop': [20, 30, 380, 470],
            'parent_rotation': 90, 'pages': entries}
