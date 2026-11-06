#!/usr/bin/env python3
"""Generate tiny original CC0 PDF fixtures for the library metadata operation.

No application launch, bundle layout, browser runner, installed PDF tool or user profile.
"""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / 'fixtures/library'

def document(objects):
    data = bytearray(b'%PDF-1.7\n%\xe2\xe3\xcf\xd3\n')
    offsets = [0]
    for number, body in enumerate(objects, 1):
        offsets.append(len(data))
        data.extend(f'{number} 0 obj\n'.encode() + body + b'\nendobj\n')
    xref = len(data)
    data.extend(f'xref\n0 {len(offsets)}\n0000000000 65535 f \n'.encode())
    for offset in offsets[1:]:
        data.extend(f'{offset:010d} 00000 n \n'.encode())
    data.extend(f'trailer\n<< /Size {len(offsets)} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode())
    return bytes(data)

def stream(dictionary, data):
    return b'<< ' + dictionary + f' /Length {len(data)} >>\nstream\n'.encode() + data + b'\nendstream'

def main():
    OUT.mkdir(exist_ok=True)
    xmp = b'''<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:prism="http://prismstandard.org/namespaces/basic/2.0/"><dc:title><rdf:Alt><rdf:li xml:lang="x-default">XMP suggested synthetic title</rdf:li></rdf:Alt></dc:title><dc:creator><rdf:Seq><rdf:li>Synthetic First</rdf:li><rdf:li>Synthetic Second</rdf:li></rdf:Seq></dc:creator><prism:doi>10.1234/EMBEDDED</prism:doi></rdf:Description></rdf:RDF></x:xmpmeta>'''
    common = [b'<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>',
              b'<< /Type /Pages /Kids [3 0 R] /Count 1 >>',
              b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 500] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>',
              stream(b'', b'BT /F1 12 Tf 40 400 Td (Original synthetic metadata fixture) Tj ET'),
              b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>',
              b'<< /Title (Info suggested title) /Author (Synthetic Author) /Subject (10.1234/INFO) >>',
              stream(b'/Type /Metadata /Subtype /XML', xmp)]
    (OUT / 'metadata.pdf').write_bytes(document(common))
    common[0] = b'<< /Type /Catalog /Pages 2 0 R >>'
    (OUT / 'info.pdf').write_bytes(document(common[:6]))
    scanned = [b'<< /Type /Catalog /Pages 2 0 R >>', common[1],
               b'<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 500] /Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>',
               stream(b'', b'q 200 0 0 200 40 40 cm /Im1 Do Q'),
               stream(b'/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8', b'\x00\x00\xff'),
               b'<< /Producer (Original CC0 synthetic image-only fixture) >>']
    (OUT / 'scanned.pdf').write_bytes(document(scanned))
    manifest = {'license': 'CC0-1.0 original generated content', 'generator': 'scripts/generate_library_fixtures.py',
                'files': {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in sorted(OUT.glob('*.pdf'))}}
    (OUT / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')

if __name__ == '__main__':
    main()
