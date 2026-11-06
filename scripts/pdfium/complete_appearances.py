#!/usr/bin/env python3
"""EXPORT ONLY PoC fallback: add explicit normal appearances to PDFium-created annotations.
No extraction/rendering replacement. Supports this PoC's axis-aligned Z-order quads and note icons.
Never modifies source; refuses existing output. See docs/ADR/pdfium-runtime.md for production limits.
"""
import argparse
from pathlib import Path
from pypdf import PdfReader, PdfWriter
from pypdf.generic import NameObject, DictionaryObject, ArrayObject, FloatObject, DecodedStreamObject

def numbers(values):
    return ArrayObject([FloatObject(v) for v in values])

def complete(source, destination):
    reader = PdfReader(source)
    writer = PdfWriter(clone_from=reader)
    count = 0
    for page in writer.pages:
        for ref in page.get('/Annots', []):
            annotation = ref.get_object()
            if annotation.get('/AP'):
                raise ValueError('existing appearance must not be overwritten')
            left, bottom, right, top = map(float, annotation['/Rect'])
            if annotation['/Subtype'] == '/Highlight':
                points = list(map(float, annotation['/QuadPoints']))
                if not points or len(points) % 8:
                    raise ValueError('invalid quads')
                commands = ['q /GS0 gs 1 1 0 rg']
                for i in range(0, len(points), 8):
                    x1,y1,x2,y2,x3,y3,x4,y4 = points[i:i+8]
                    if not (abs(y1-y2)<0.01 and abs(y3-y4)<0.01 and abs(x1-x3)<0.01 and abs(x2-x4)<0.01):
                        raise ValueError('PoC export supports axis-aligned Z-order quads only')
                    commands.append(f'{x1} {y1} m {x2} {y2} l {x4} {y4} l {x3} {y3} l h f')
                commands.append('Q')
                annotation[NameObject('/C')] = numbers([1,1,0])
                annotation[NameObject('/CA')] = FloatObject(0.5)
            elif annotation['/Subtype'] == '/Text':
                commands = [f'q 1 0.8 0 rg {left} {bottom} {right-left} {top-bottom} re f',
                    f'0 G 1 w {left+1} {bottom+1} {right-left-2} {top-bottom-2} re S',
                    f'{left+4} {bottom+6} m {right-4} {bottom+6} l S',
                    f'{left+4} {bottom+10} m {right-4} {bottom+10} l S Q']
            else:
                raise ValueError('unsupported annotation type')
            appearance = DecodedStreamObject()
            appearance.set_data(('\n'.join(commands)+'\n').encode('ascii'))
            appearance.update({NameObject('/Type'):NameObject('/XObject'),NameObject('/Subtype'):NameObject('/Form'),
                NameObject('/FormType'):FloatObject(1),NameObject('/BBox'):numbers([left,bottom,right,top]),
                NameObject('/Matrix'):numbers([1,0,0,1,0,0]),
                NameObject('/Resources'):DictionaryObject({NameObject('/ExtGState'):DictionaryObject({NameObject('/GS0'):DictionaryObject({
                    NameObject('/Type'):NameObject('/ExtGState'),NameObject('/ca'):FloatObject(0.5),NameObject('/CA'):FloatObject(0.5),
                    NameObject('/BM'):NameObject('/Multiply')})})})})
            annotation[NameObject('/AP')] = DictionaryObject({NameObject('/N'):writer._add_object(appearance)})
            count += 1
    with destination.open('xb') as out:
        writer.write(out)
    print(f'Export-only explicit appearances: {count} annotations -> {destination}')

if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    complete(args.source, args.destination)
