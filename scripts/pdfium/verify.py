#!/usr/bin/env python3
"""Fail-closed independent PDFKit measurements and PDF structure validation.
Run after run.py. No PDFium API used here. Pixel positions checked against generated PDF space.
"""
import hashlib
import json
import math
from pathlib import Path
import sys
from PIL import Image
from pypdf import PdfReader

if not __debug__:
    raise RuntimeError('verification requires assertions enabled; do not run Python with -O')

ROOT = Path(__file__).resolve().parents[2]

def near(a, b, tolerance=0.02):
    assert len(a) == len(b)
    error = max(abs(float(x)-float(y)) for x,y in zip(a,b))
    assert error <= tolerance, (a,b,error)
    return error

def display(x,y,rotation):
    # Independent explicit fixture expectations: CropBox [20,30,380,470], 1 pixel/point.
    return {0:(x-20,470-y),90:(y-30,x-20),180:(380-x,y-30),270:(470-y,380-x)}[rotation]

def bounds(quad):
    return [min(p['x'] for p in quad),min(p['y'] for p in quad),max(p['x'] for p in quad),max(p['y'] for p in quad)]

def check(out, inherited=False):
    pdfium = json.loads((out/'pdfium-report.json').read_text())
    independent = json.loads((out/'pdfkit-portable/report.json').read_text())
    # The export-only fallback refuses existing appearances rather than silently rewriting them.
    from complete_appearances import complete
    rejected = out/'must-not-be-created.pdf'
    try:
        complete(out/'portable-export.pdf', rejected)
    except ValueError as error:
        assert 'existing appearance' in str(error)
    else:
        raise AssertionError('existing appearances unexpectedly overwritten')
    assert not rejected.exists()
    final = PdfReader(out/'portable-export.pdf')
    raw = PdfReader(out/'pdfium-export.pdf')
    fixture = json.loads((ROOT/'fixtures/pdf/manifest.json').read_text())
    for name, expected in fixture['files'].items():
        assert hashlib.sha256((ROOT/'fixtures/pdf'/name).read_bytes()).hexdigest() == expected
    source_name = 'inherited.pdf' if inherited else 'corpus.pdf'
    assert hashlib.sha256((ROOT/'fixtures/pdf'/source_name).read_bytes()).hexdigest() == pdfium['original_sha256']
    page_count = 2 if inherited else 6
    rotations = fixture['inherited']['rotations'] if inherited else fixture['rotations'][:4]
    assert len(final.pages) == page_count and len(independent['pages']) == page_count
    # Both engines must preserve the effective crop/rotation through native and completed export.
    # Expectations come from synthetic PDF-space constants, not from the adapter's Viewport.
    for name in ['original', 'native', 'portable']:
        report = json.loads((out/f'pdfkit-{name}/report.json').read_text())
        assert len(report['pages']) == page_count
        for i, rotation in enumerate(rotations):
            p = report['pages'][i]
            near(p['crop'], [20, 30, 380, 470])
            assert p['rotation'] == rotation
            assert [p['width'], p['height']] == ([440, 360] if rotation in (90, 270) else [360, 440])
            assert fixture['unicode_line'] in p['text'] and fixture['second_line'] in p['text']
            assert len(p['annotations']) == (0 if name == 'original' else 2)
            if name != 'original':
                h = next(a for a in p['annotations'] if a['type'] == 'Highlight')
                n = next(a for a in p['annotations'] if a['type'] == 'Text')
                assert h['contents'] == fixture['highlight_comment'] and n['contents'] == fixture['note']
                near(n['bounds'], [300, 300, 324, 324])
                if inherited:
                    quads = [[{'x':l,'y':t},{'x':r,'y':t},{'x':l,'y':b},{'x':r,'y':b}]
                             for l,b,r,t in fixture['inherited']['highlight_rects']]
                else:
                    quads = pdfium['highlights'][i]['quads']
                expected_points = [q for quad in quads for q in quad]
                assert len(h['quads_absolute']) == len(expected_points) == 8
                for point, q in zip(h['quads_absolute'], expected_points):
                    near(point, [q['x'], q['y']])
                near(h['color'][:3], [1,1,0])
            raster = Image.open(out/f'pdfkit-{name}/page-{i}.png').convert('RGB')
            red = [(x+0.5,y+0.5) for y in range(raster.height) for x in range(raster.width)
                   if (lambda c: c[0]>245 and c[1]<10 and c[2]<10)(raster.getpixel((x,y)))]
            assert len(red) == 96
            near([sum(p[0] for p in red)/96, sum(p[1] for p in red)/96], display(46,54,rotation),0.5)
    results=[]
    for i, rotation in enumerate(rotations):
        actual = independent['pages'][i]
        expected = pdfium['highlights'][i]
        if inherited:
            # Deliberately fixed rectangles in the generator, independent of native glyph metrics.
            expected = {'quads': [[{'x':l,'y':t},{'x':r,'y':t},{'x':l,'y':b},{'x':r,'y':b}]
                                  for l,b,r,t in fixture['inherited']['highlight_rects']]}
            for actual_quad, expected_quad in zip(pdfium['highlights'][i]['quads'], expected['quads']):
                for p,q in zip(actual_quad,expected_quad):
                    near([p['x'],p['y']],[q['x'],q['y']])
        assert actual['page_index'] == i and actual['rotation'] == rotation
        if not inherited:
            assert actual['label'] == ('iv' if i == 0 else 'S12')
        near(actual['crop'],fixture['crop'])
        assert fixture['unicode_line'] in actual['text'] and fixture['second_line'] in actual['text']
        assert len(actual['annotations']) == 2
        highlight = next(a for a in actual['annotations'] if a['type'] == 'Highlight')
        note = next(a for a in actual['annotations'] if a['type'] == 'Text')
        assert highlight['contents'] == fixture['highlight_comment'] and note['contents'] == fixture['note']
        near(note['bounds'],[300,300,324,324])
        expected_points = [p for q in expected['quads'] for p in q]
        max_quad_error = max(near(p,[q['x'],q['y']]) for p,q in zip(highlight['quads_absolute'],expected_points))
        assert len(highlight['quads_absolute']) == len(expected_points) == 8
        near(highlight['color'][:3],[1,1,0])
        # PDFKit selection boxes independently check each PDFium character's PDF-space loose box.
        chars = [c for c in pdfium['pages'][i]['characters'] if c['unicode'] and not c['unicode'].isspace()]
        assert len(chars) == len(actual['glyphs'])
        char_error = 0
        remaining = list(actual['glyphs'])
        for c in chars:
            box = bounds(c['quad'])
            candidates = [g for g in remaining if g['text'] == c['unicode']]
            assert candidates, c
            g = min(candidates, key=lambda g: sum(abs(a-b) for a,b in zip(box,g['bounds'])))
            remaining.remove(g)
            # PDFKit font-advance selection vs PDFium loose ink extents: descenders extend
            # up to 1.512pt below PDFKit's selection baseline; f/r overhang up to 0.434pt.
            near([box[0],box[2],box[3]],[g['bounds'][0],g['bounds'][2],g['bounds'][3]],0.5)
            char_error = max(char_error,near(box,g['bounds'],tolerance=2.0))
        assert not remaining
        # Verify persisted AP forms for both highlight and independent note, not a viewer-only synthesis.
        for raw_ref,ref in zip(raw.pages[i]['/Annots'],final.pages[i]['/Annots']):
            assert '/AP' not in raw_ref.get_object()
            a=ref.get_object()
            assert a['/AP']['/N']['/Subtype'] == '/Form'
            assert len(a['/AP']['/N'].get_data()) > 30
            if a['/Subtype'] == '/Highlight':
                near(a['/C'],[1,1,0]); assert float(a['/CA']) == 0.5
                near(a['/QuadPoints'],[v for p in expected_points for v in (p['x'],p['y'])])
        image = Image.open(out/f'pdfkit-portable/page-{i}.png').convert('RGB')
        original = Image.open(out/f'pdfkit-original/page-{i}.png').convert('RGB')
        engine = Image.open(out/f'pdfium-portable/page-{i}.ppm').convert('RGB')
        engine_original = Image.open(out/f'original-{i}.ppm').convert('RGB')
        assert image.size == original.size == engine.size
        marker=[];yellow=set();engine_yellow=set();mask=set()
        for y in range(image.height):
            for x in range(image.width):
                r,g,b=image.getpixel((x,y))
                er,eg,eb=engine.getpixel((x,y))
                if r>245 and g<10 and b<10: marker.append((x+0.5,y+0.5))
                if r>240 and g>240 and b<230: yellow.add((x,y))
                if er>240 and eg>240 and eb<230: engine_yellow.add((x,y))
        assert len(marker)==96
        center=(sum(x for x,y in marker)/96,sum(y for x,y in marker)/96)
        marker_error=near(center,display(46,54,rotation),tolerance=0.5)
        # Exclude text ink and anti-aliased boundaries: check interiors away from edges.
        for q in expected['quads']:
            points=[display(p['x'],p['y'],rotation) for p in q]
            x0=math.ceil(min(x for x,y in points)+2);x1=math.floor(max(x for x,y in points)-2)
            y0=math.ceil(min(y for x,y in points)+2);y1=math.floor(max(y for x,y in points)-2)
            for y in range(y0,y1):
                for x in range(x0,x1):
                    if min(original.getpixel((x,y)))>248 and min(engine_original.getpixel((x,y)))>248: mask.add((x,y))
        coverage=len(mask & yellow)/len(mask)
        engine_coverage=len(mask & engine_yellow)/len(mask)
        assert coverage>0.98 and engine_coverage>0.98, (i,coverage,engine_coverage)
        # No stray yellow outside highlight polygons and the native 24pt note icon (2px margin).
        def expected_yellow(p):
            x,y=p
            boxes=[bounds(q) for q in expected['quads']]+[[300,300,324,324]]
            for l,b,r,t in boxes:
                corners=[display(l,b,rotation),display(r,t,rotation)]
                if min(a for a,b in corners)-2<=x<=max(a for a,b in corners)+2 and min(b for a,b in corners)-2<=y<=max(b for a,b in corners)+2:return True
            return False
        assert all(expected_yellow(p) for p in yellow)
        corners=[display(300,300,rotation),display(324,324,rotation)]
        note_counts=[]
        for raster in [image,engine]:
            count=0
            for y in range(int(min(y for x,y in corners)),int(max(y for x,y in corners))):
                for x in range(int(min(x for x,y in corners)),int(max(x for x,y in corners))):
                    r,g,b=raster.getpixel((x,y))
                    count += r>160 and g>100 and b<230
            assert count>100, ('note icon not rendered in expected bounds',i,count)
            note_counts.append(count)
        results.append({'page_index':i,'rotation':rotation,'quads':2,'checked_characters':len(chars),
            'max_quad_error_pt':max_quad_error,'max_char_box_error_pt':char_error,
            'marker_centroid_error_px':marker_error,'PDFKit_yellow_interior_coverage':coverage,
            'PDFium_yellow_interior_coverage':engine_coverage,'PDFKit_yellow_pixels':len(yellow),'note_colored_pixels_PDFKit_PDFium':note_counts})
    if not inherited:
        assert independent['pages'][4]['text'] == '' and independent['pages'][4]['annotations'] == []
    report={'status':'passed','independent_renderer':independent['renderer'],'os':independent['os'],
        'corpus':source_name,'appearance_forms':2*len(rotations),'original_unchanged':True,'checks':results,
        'observed_native_limit':'PDFium export has no /AP; explicit export-only pypdf completion verified',
        'portable_sha256':hashlib.sha256((out/'portable-export.pdf').read_bytes()).hexdigest()}
    (out/'verification.json').write_text(json.dumps(report,ensure_ascii=False,indent=2)+'\n')
    print(json.dumps(report,ensure_ascii=False,indent=2))

if __name__ == '__main__':
    check(Path(sys.argv[1]))
