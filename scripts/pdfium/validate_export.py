#!/usr/bin/env python3
"""#20 independent check of the annotated export, without PDFium.

Reads the directory left by `cargo test -p cm-pdf --features runtime --test export_runtime`
(`target/pdf-export-test-<pid>/`: export.pdf, legacy.pdf, legacy-export.pdf, expected.json).

* pypdf (structure): /Subtype, /NM, /Contents (Unicode), /QuadPoints, /Rect, /C, /CA, print
  flag and a persistent /AP /N Form whose /BBox is the /Rect; source annotations unchanged
  (the AP-less legacy Highlight still has no /AP and keeps its colour); source hash.
* poppler `pdftoppm` (rendering, optional with --no-render): highlight and note pixels at the
  expected display position of every rotated, cropped page.

Development/validation only: needs `pypdf` and Pillow (scripts/pdfium/requirements.txt) and
poppler-utils. Not part of the default test lane; the product never runs Python.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

if not __debug__:
    raise RuntimeError('verification requires assertions enabled; do not run Python with -O')

TOLERANCE = 0.01


def near(a, b, tolerance=TOLERANCE):
    a, b = [float(x) for x in a], [float(x) for x in b]
    assert len(a) == len(b), (a, b)
    error = max((abs(x - y) for x, y in zip(a, b)), default=0.0)
    assert error <= tolerance, (a, b, error)
    return error


def display(x, y, rotation, crop):
    """Unrotated PDF point -> top-left display pixel at 72 dpi, as poppler renders the CropBox."""
    left, bottom, right, top = crop
    w, h = right - left, top - bottom
    x, y = x - left, y - bottom
    return {0: (x, h - y), 90: (y, x), 180: (w - x, y), 270: (h - y, w - x)}[rotation]


def annots(page):
    return [a.get_object() for a in page.get('/Annots', [])]


def check_new(page_annots, expected, kind):
    found = [a for a in page_annots if a.get('/NM') == expected['name']]
    assert len(found) == 1, (expected['name'], len(found))
    a = found[0]
    assert a['/Subtype'] == ('/Highlight' if kind == 'highlight' else '/Text'), a['/Subtype']
    assert int(a.get('/F', 0)) & 4, 'print flag'
    assert str(a.get('/Contents', '')) == expected['contents'], (a.get('/Contents'), expected['contents'])
    near(a['/C'], [c / 255 for c in expected['color']])
    ap = a['/AP']['/N'].get_object()
    assert ap['/Subtype'] == '/Form' and len(ap.get_data()) > 20, 'persistent normal appearance'
    near(ap['/BBox'], a['/Rect'])
    errors = []
    if kind == 'highlight':
        near([a.get('/CA', 1)], [expected['opacity']], 1 / 255)
        points = [v for quad in expected['quads'] for p in quad for v in p]
        errors.append(near(a['/QuadPoints'], points))
        xs, ys = points[0::2], points[1::2]
        errors.append(near(a['/Rect'], [min(xs), min(ys), max(xs), max(ys)]))
        assert '/GS' in ap['/Resources']['/ExtGState'], 'opacity graphics state'
    else:
        errors.append(near(a['/Rect'], expected['rect']))
    return max(errors)


def check_structure(pdf, expected, preserved_from=None):
    from pypdf import PdfReader
    reader = PdfReader(pdf)
    max_error = 0.0
    added = 0
    for h in expected['highlights']:
        max_error = max(max_error, check_new(annots(reader.pages[h['page']]), h, 'highlight'))
        added += 1
    for n in expected['notes']:
        max_error = max(max_error, check_new(annots(reader.pages[n['page']]), n, 'note'))
        added += 1
    total = sum(len(annots(p)) for p in reader.pages)
    preserved = 0
    if preserved_from is not None:
        source = PdfReader(preserved_from)
        for old_page, new_page in zip(source.pages, reader.pages):
            old, new = annots(old_page), annots(new_page)
            assert len(new) >= len(old)
            for a, b in zip(old, new):
                # Same dictionary entries; an AP-less annotation stays AP-less (no recolouring).
                assert set(a.keys()) - {'/P'} == set(b.keys()) - {'/P'}, (a.keys(), b.keys())
                for key in a.keys() - {'/P', '/AP'}:
                    assert a[key] == b[key], (key, a[key], b[key])
                assert ('/AP' in a) == ('/AP' in b)
                preserved += 1
    assert total == added + preserved, (total, added, preserved)
    return {'annotations': total, 'added': added, 'preserved': preserved,
            'max_position_error_pt': max_error, 'pages': len(reader.pages)}


def render(pdf, page, out):
    target = out / f'{pdf.stem}-{page}'
    subprocess.run(['pdftoppm', '-cropbox', '-r', '72', '-png', '-singlefile', '-f', str(page + 1),
                    '-l', str(page + 1), str(pdf), str(target)], check=True, timeout=120)
    from PIL import Image
    return Image.open(f'{target}.png').convert('RGB')


def coverage(image, rect, rotation, crop, color):
    l, b, r, t = rect
    (x0, y0), (x1, y1) = display(l, b, rotation, crop), display(r, t, rotation, crop)
    return coverage_box(image, (x0, y0, x1, y1), color)


def upright_icon_box(rect, rotation, crop):
    """Poppler draws Text annotation icons upright (as if NoRotate were set), pinned at the
    display position of the /Rect's unrotated upper-left corner. PDFium and PDFKit (M0) rotate
    the icon with the page instead; for /Rotate 0 both are the /Rect itself."""
    l, b, r, t = rect
    x, y = display(l, t, rotation, crop)
    return (x, y, x + (r - l), y + (t - b))


def coverage_box(image, box, color):
    x0, y0, x1, y1 = box
    hits = total = 0
    for y in range(int(min(y0, y1)) + 2, int(max(y0, y1)) - 1):
        for x in range(int(min(x0, x1)) + 2, int(max(x0, x1)) - 1):
            total += 1
            hits += all(abs(c - e) <= 24 for c, e in zip(image.getpixel((x, y)), color))
    return hits / max(total, 1)


def check_render(pdf, expected, pages, out):
    results = []
    for h in expected['highlights']:
        page = pages[h['page']]
        image = render(pdf, h['page'], out)
        blend = [255 - (255 - c) * h['opacity'] for c in h['color']]
        lines = []
        for quad in h['quads']:
            xs, ys = [p[0] for p in quad], [p[1] for p in quad]
            lines.append(coverage(image, [min(xs), min(ys), max(xs), max(ys)],
                                  page['rotation'], page['crop'], blend))
        assert min(lines) > 0.2, (h['name'], lines)
        results.append({'name': h['name'], 'rotation': page['rotation'], 'line_coverage': lines})
    for n in expected['notes']:
        page = pages[n['page']]
        image = render(pdf, n['page'], out)
        icon = coverage_box(image, upright_icon_box(n['rect'], page['rotation'], page['crop']),
                            n['color'])
        assert icon > 0.4, (n['name'], icon)
        results.append({'name': n['name'], 'rotation': page['rotation'], 'icon_coverage': icon})
    return results


def main(directory, render_pages):
    expected = json.loads((directory / 'expected.json').read_text())
    source = Path(expected['source'])
    assert hashlib.sha256(source.read_bytes()).hexdigest() == expected['source_sha256'], 'original changed'
    report = {
        'export': check_structure(directory / 'export.pdf', expected['export'], source),
        'legacy': check_structure(directory / 'legacy-export.pdf', expected['legacy']['export'],
                                  Path(expected['legacy']['source'])),
        'original_sha256_unchanged': True,
    }
    assert report['legacy']['preserved'] == len(expected['legacy']['preserved'])
    if render_pages:
        if shutil.which('pdftoppm') is None:
            raise RuntimeError('pdftoppm (poppler-utils) not found; pass --no-render to skip rendering')
        with tempfile.TemporaryDirectory() as tmp:
            pages = {p['index']: p for p in expected['pages']}
            report['poppler'] = check_render(directory / 'export.pdf', expected['export'], pages,
                                             Path(tmp))
            version = subprocess.run(['pdftoppm', '-v'], capture_output=True, text=True)
            report['poppler_version'] = (version.stderr or version.stdout).splitlines()[0]
    report['status'] = 'passed'
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('directory', type=Path)
    parser.add_argument('--no-render', action='store_true')
    args = parser.parse_args()
    main(args.directory.resolve(), not args.no_render)
    sys.exit(0)
