#!/usr/bin/env python3
"""Generate only original synthetic test content, CC0; embedded Noto Sans JP is OFL.
Requires pinned requirements.txt. No private papers or system fonts.
"""
import argparse
import hashlib
import io
import json
from pathlib import Path
from fontTools.ttLib import TTFont as Font
from fontTools.varLib.instancer import instantiateVariableFont
from reportlab.pdfgen.canvas import Canvas
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.lib.utils import ImageReader
from PIL import Image
from pypdf import PdfReader, PdfWriter
from pypdf.generic import DictionaryObject, ArrayObject, NumberObject, TextStringObject, NameObject

FONT_SHA = 'c2f3b4d463500a2ddcd3849cded1fceeb9fd6d1c32e6cbecd568453ba50fc68f'
if not __debug__:
    raise RuntimeError('verification requires assertions enabled; do not run Python with -O')

ROOT = Path(__file__).resolve().parents[2]

def generate(font_path, output):
    assert hashlib.sha256(font_path.read_bytes()).hexdigest() == FONT_SHA, 'font checksum mismatch'
    static = instantiateVariableFont(Font(font_path), {'wght': 400}, inplace=False)
    static.recalcTimestamp = False
    font = io.BytesIO()
    static.save(font)
    font.seek(0)
    pdfmetrics.registerFont(TTFont('CMFixtureNotoJP', font))
    data = io.BytesIO()
    c = Canvas(data, pagesize=(400, 500), invariant=1, pageCompression=1)
    c.setTitle('Original synthetic PDFium validation corpus')
    c.setAuthor('citation-manager contributors; CC0-1.0 synthetic content; Noto OFL')
    for rotation in (0, 90, 180, 270):
        c.setFont('CMFixtureNotoJP', 14)
        c.drawString(45, 410, 'Unicode 日本語 αβ café')
        c.drawString(45, 382, 'second line: coordinate proof')
        # Exact interior solid red marker, used for physical pixel/coordinate assertions.
        c.setFillColorRGB(1, 0, 0)
        c.rect(40, 50, 12, 8, fill=1, stroke=0)
        c.setFillColorRGB(0, 0, 0)
        c.showPage()
    image = Image.new('RGB', (16, 16), (0, 0, 255))
    c.drawImage(ImageReader(image), 60, 60, 200, 200)
    c.showPage()
    c.setFont('CMFixtureNotoJP', 12)
    c.drawString(21, 455, 'top boundary')
    c.drawString(21, 34, 'bottom boundary')
    c.showPage()
    c.save()
    reader = PdfReader(io.BytesIO(data.getvalue()))
    writer = PdfWriter()
    for i, page in enumerate(reader.pages):
        page.cropbox.lower_left = (20, 30)
        page.cropbox.upper_right = (380, 470)
        if i < 4:
            page.rotate(i * 90)
        writer.add_page(page)
    writer._root_object[NameObject('/PageLabels')] = DictionaryObject({
        NameObject('/Nums'): ArrayObject([NumberObject(0), DictionaryObject({NameObject('/P'): TextStringObject('iv')}),
            NumberObject(1), DictionaryObject({NameObject('/P'): TextStringObject('S12')})])})
    output.mkdir(parents=True, exist_ok=True)
    with (output/'corpus.pdf').open('wb') as f:
        writer.write(f)
    encrypted = PdfWriter()
    encrypted.add_page(reader.pages[0])
    encrypted.encrypt('test-password', 'owner-password', algorithm='RC4-128')
    with (output/'encrypted.pdf').open('wb') as f:
        encrypted.write(f)
    (output/'malformed.pdf').write_bytes(b'%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 99 0 R >>\n%%EOF\n')
    # Mutate only writer-owned raw dictionaries AFTER add_page(). Reading reader.pages
    # or adding a page can flatten inherited attributes; nothing may do that below.
    inherited = PdfWriter()
    for page in reader.pages[:2]:
        inherited.add_page(page)
    parent = inherited._pages.get_object()
    parent[NameObject('/MediaBox')] = ArrayObject(map(NumberObject, [0, 0, 400, 500]))
    parent[NameObject('/CropBox')] = ArrayObject(map(NumberObject, [20, 30, 380, 470]))
    parent[NameObject('/Rotate')] = NumberObject(90)
    for i, ref in enumerate(parent['/Kids']):
        page = ref.get_object()
        for key in ['/MediaBox', '/CropBox', '/Rotate']:
            page.pop(key, None)
        if i == 1:
            page[NameObject('/MediaBox')] = ArrayObject(map(NumberObject, [0, 0, 400, 500]))
    with (output/'inherited.pdf').open('wb') as f:
        inherited.write(f)
    from inherited_boxes import inspect_structure
    inspect_structure(output/'inherited.pdf')
    manifest = {
        'license': 'CC0-1.0 for synthetic content; embedded font SIL-OFL-1.1',
        'generator': 'scripts/pdfium/generate_fixtures.py',
        'font_sha256': FONT_SHA,
        'files': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(output.glob('*.pdf'))},
        'inherited': {'file': 'inherited.pdf', 'rotations': [90, 90],
                      'direct_entries': [[], ['/MediaBox']], 'render_size_at_zoom_1': [440, 360],
                      'highlight_rects': [[44, 406, 330, 426], [44, 378, 330, 398]]},
        'crop': [20, 30, 380, 470], 'rotations': [0, 90, 180, 270, 0, 0],
        'marker': [40, 50, 52, 58], 'unicode_line': 'Unicode 日本語 αβ café',
        'second_line': 'second line: coordinate proof', 'image_only_page_index': 4,
        'password': 'test-password', 'highlight_comment': '検証コメント αβ café', 'note': '独立ノート 日本語',
    }
    (output/'manifest.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2)+'\n')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--font', type=Path, required=True)
    parser.add_argument('--output', type=Path, default=ROOT/'fixtures/pdf')
    args = parser.parse_args()
    generate(args.font, args.output)
