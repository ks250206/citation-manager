"""Deterministic, standard-library-only synthetic PDF writer (#30).

Used by `scripts/generate_library_fixtures.py` (the committed hard-layout fixture) and by
the representative benchmark corpus (`representative.py`). All text is original synthetic
content (CC0-1.0); no font program is embedded and nothing is downloaded.

Fonts: `/F1` Helvetica and `/F3` Symbol (standard 14, not embedded) and two Type0 fonts
whose 2-byte codes are the Unicode BMP scalar: `/F2` Identity-H (horizontal) with an
identity ToUnicode CMap, and `/F4` UniJIS-UCS2-V (vertical, WMode 1, predefined CMap).
Text extraction therefore does not depend on any installed CJK font. Glyph appearance of the Type0 text
falls back to whatever the renderer substitutes; only the text layer is asserted.
"""
import hashlib
import random
import zlib

HEADER = b'%PDF-1.7\n%\xe2\xe3\xcf\xd3\n'


def _identity_to_unicode():
    lines = [b'/CIDInit /ProcSet findresource begin', b'12 dict begin', b'begincmap',
             b'/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def',
             b'/CMapName /Synthetic-Identity-UCS def', b'/CMapType 2 def',
             b'1 begincodespacerange', b'<0000> <FFFF>', b'endcodespacerange']
    # bfrange source ranges may only vary in the last byte: one range per high byte,
    # surrogates excluded, at most 100 entries per block.
    highs = [h for h in range(256) if not 0xD8 <= h <= 0xDF]
    for start in range(0, len(highs), 100):
        block = highs[start:start + 100]
        lines.append(f'{len(block)} beginbfrange'.encode())
        for h in block:
            lines.append(f'<{h:02X}00> <{h:02X}FF> <{h:02X}00>'.encode())
        lines.append(b'endbfrange')
    lines += [b'endcmap', b'CMapName currentdict /CMap defineresource pop', b'end', b'end']
    return b'\n'.join(lines)


TO_UNICODE = _identity_to_unicode()


def utf16_hex(text):
    """Type0 identity code string (one 2-byte code per BMP scalar)."""
    for ch in text:
        if ord(ch) > 0xFFFF or 0xD800 <= ord(ch) <= 0xDFFF:
            raise ValueError('BMP scalar values only')
    return '<' + ''.join(f'{ord(ch):04X}' for ch in text) + '>'


def latin(text):
    """PDF literal string for a standard-14 font (printable ASCII only)."""
    if any(not 32 <= ord(ch) < 127 for ch in text):
        raise ValueError('ASCII only for standard fonts')
    return '(' + text.replace('\\', '\\\\').replace('(', '\\(').replace(')', '\\)') + ')'


class Document:
    """Collects page content streams and serialises a classic xref PDF byte-for-byte stably."""

    def __init__(self, media_box=(0, 0, 595, 842), compress=True):
        self.media_box = media_box
        self.compress = compress
        self.pages = []  # (content bytes, rotate)

    def add_page(self, content, rotate=0):
        self.pages.append((content.encode('ascii') if isinstance(content, str) else content, rotate))

    def _stream(self, dictionary, data, compress):
        if compress:
            data = zlib.compress(data, 9)
            dictionary += b' /Filter /FlateDecode'
        return b'<< ' + dictionary + f' /Length {len(data)} >>\nstream\n'.encode() + data + b'\nendstream'

    def to_bytes(self):
        # 1 catalog, 2 pages, 3-6 fonts, 7 CIDFont, 8 ToUnicode, 9.. page/content pairs.
        objects = [None] * 8
        objects[0] = b'<< /Type /Catalog /Pages 2 0 R >>'
        objects[2] = b'<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>'
        objects[3] = (b'<< /Type /Font /Subtype /Type0 /BaseFont /SyntheticUnicode /Encoding /Identity-H '
                      b'/DescendantFonts [7 0 R] /ToUnicode 8 0 R >>')
        objects[4] = b'<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>'
        # Vertical: the predefined UniJIS-UCS2-V CMap (UCS-2 codes, Adobe-Japan1 CIDs), the
        # classic non-embedded Japanese setup. PDFium 8125 drops Identity-V text of a
        # non-embedded font even with a ToUnicode CMap, so that variant is not used.
        objects[5] = (b'<< /Type /Font /Subtype /Type0 /BaseFont /HeiseiMin-W3 /Encoding /UniJIS-UCS2-V '
                      b'/DescendantFonts [<< /Type /Font /Subtype /CIDFontType0 /BaseFont /HeiseiMin-W3 '
                      b'/CIDSystemInfo << /Registry (Adobe) /Ordering (Japan1) /Supplement 2 >> '
                      b'/FontDescriptor << /Type /FontDescriptor /FontName /HeiseiMin-W3 /Flags 6 '
                      b'/FontBBox [-123 -257 1001 910] /ItalicAngle 0 /Ascent 723 /Descent -241 '
                      b'/CapHeight 709 /StemV 69 >> /DW 1000 >>] >>')
        objects[6] = (b'<< /Type /Font /Subtype /CIDFontType2 /BaseFont /SyntheticUnicode '
                      b'/CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> '
                      b'/FontDescriptor << /Type /FontDescriptor /FontName /SyntheticUnicode /Flags 4 '
                      b'/FontBBox [0 -120 1000 880] /ItalicAngle 0 /Ascent 880 /Descent -120 '
                      b'/CapHeight 700 /StemV 80 >> /DW 1000 /CIDToGIDMap /Identity >>')
        objects[7] = self._stream(b'', TO_UNICODE, self.compress)
        resources = b'/Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 5 0 R /F4 6 0 R >> >>'
        kids = []
        box = ' '.join(str(v) for v in self.media_box).encode()
        for content, rotate in self.pages:
            page_number = len(objects) + 1
            kids.append(f'{page_number} 0 R'.encode())
            rotation = f' /Rotate {rotate}'.encode() if rotate else b''
            objects.append(b'<< /Type /Page /Parent 2 0 R /MediaBox [' + box + b'] ' + resources
                           + rotation + f' /Contents {page_number + 1} 0 R >>'.encode())
            objects.append(self._stream(b'', content, self.compress))
        objects[1] = (b'<< /Type /Pages /Kids [' + b' '.join(kids) + b'] '
                      + f'/Count {len(self.pages)} >>'.encode())
        data = bytearray(HEADER)
        offsets = []
        for number, body in enumerate(objects, 1):
            offsets.append(len(data))
            data.extend(f'{number} 0 obj\n'.encode() + body + b'\nendobj\n')
        xref = len(data)
        data.extend(f'xref\n0 {len(objects) + 1}\n0000000000 65535 f \n'.encode())
        for offset in offsets:
            data.extend(f'{offset:010d} 00000 n \n'.encode())
        data.extend(f'trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n'.encode())
        return bytes(data)


def text_line(font, size, x, y, text):
    shown = utf16_hex(text) if font in ('F2', 'F4') else latin(text)
    return f'BT /{font} {size} Tf {x} {y} Td {shown} Tj ET\n'


# ----------------------------------------------------------------- hard-layout fixture

LAYOUT_MARKERS = {
    'two_column_left': ['LEFTCOL alpha one', 'LEFTCOL alpha two', 'LEFTCOL alpha three'],
    'two_column_right': ['RIGHTCOL beta one', 'RIGHTCOL beta two', 'RIGHTCOL beta three'],
    'math_text': 'Energy relation E = mc',
    'math_symbols': 'abSp',  # Symbol font codes: alpha beta Sigma pi
    'table_cells': [['Cell r0c0', 'Cell r0c1', 'Cell r0c2'],
                    ['Cell r1c0', 'Cell r1c1', 'Cell r1c2'],
                    ['Cell r2c0', 'Cell r2c1', 'Cell r2c2']],
    'vertical_ja': '縦書きの日本語試験',
    'vertical_ja_second': '二列目の縦組',
    'horizontal_ja': '横書きの対照行です',
    'rotated': 'ROTATED page marker ninety',
}


def layouts_pdf():
    """Four pages: two-column, math, table, vertical Japanese; plus a /Rotate 90 page."""
    m = LAYOUT_MARKERS
    doc = Document(media_box=(0, 0, 595, 842))
    # Page 0: two columns, written column by column (the common producer order).
    content = ''
    for i, line in enumerate(m['two_column_left']):
        content += text_line('F1', 11, 56, 760 - 16 * i, line)
    for i, line in enumerate(m['two_column_right']):
        content += text_line('F1', 11, 320, 760 - 16 * i, line)
    content += '0.5 w 300 100 m 300 780 l S\n'  # column rule
    doc.add_page(content)
    # Page 1: math (baseline shift superscript, fraction bar, Symbol glyphs).
    content = text_line('F1', 14, 60, 700, m['math_text'])
    content += 'BT /F1 9 Tf 223 708 Td 6 Ts (2) Tj 0 Ts ET\n'
    content += text_line('F3', 14, 60, 660, m['math_symbols'])
    content += text_line('F1', 12, 110, 620, 'numerator x')
    content += '0.8 w 100 612 m 200 612 l S\n'
    content += text_line('F1', 12, 110, 596, 'denominator y')
    doc.add_page(content)
    # Page 2: a ruled 3x3 table.
    content = '0.6 w\n'
    for r in range(4):
        content += f'60 {700 - 30 * r} m 420 {700 - 30 * r} l S\n'
    for c in range(4):
        content += f'{60 + 120 * c} 700 m {60 + 120 * c} 610 l S\n'
    for r, row in enumerate(m['table_cells']):
        for c, cell in enumerate(row):
            content += text_line('F1', 10, 70 + 120 * c, 680 - 30 * r, cell)
    doc.add_page(content)
    # Page 3: vertical Japanese (Identity-V, WMode 1), two right-to-left columns, plus one
    # horizontal control line.
    content = text_line('F4', 16, 480, 780, m['vertical_ja'])
    content += text_line('F4', 16, 450, 780, m['vertical_ja_second'])
    content += text_line('F2', 12, 60, 120, m['horizontal_ja'])
    doc.add_page(content)
    # Page 4: rotated page.
    doc.add_page(text_line('F1', 12, 60, 700, m['rotated']), rotate=90)
    return doc.to_bytes()


# ----------------------------------------------------------- representative corpus

# Planted search terms. The background vocabularies below never contain their characters.
PLANTED_JA = ['電', '電池', '電解質', '固体電解質', '電子']
PLANTED_EN = ['lithium', 'dendrite', 'zirconia']
QUERY_TERMS = ['電', '電池', '電解質', '固体電解質', 'lithium', 'dendrite', 'zirconia']
_KANJI = '研究結果方法材料実験測定評価分析構造表面温度圧力試料条件反応速度変化特性理論計算模型制御設計'
_KANA = 'あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめもやゆよらりるれろわを'
_FORBIDDEN = set(''.join(PLANTED_JA))


def _english_word(rng):
    syllables = ['ka', 'to', 'mi', 're', 'su', 'no', 'va', 'pe', 'lo', 'ri', 'ne', 'go', 'ba', 'te', 'xu']
    return ''.join(rng.choice(syllables) for _ in range(rng.randint(2, 4)))


def _japanese_phrase(rng):
    phrase = ''.join(rng.choice(_KANJI) for _ in range(rng.randint(2, 4))) + rng.choice('のをにはがでと')
    phrase += ''.join(rng.choice(_KANA) for _ in range(rng.randint(1, 3)))
    assert not _FORBIDDEN & set(phrase)
    return phrase


def page_lines(seed, global_page, lines=34):
    """(font, text) lines of one corpus page; planted terms never cross a line boundary."""
    rng = random.Random(f'{seed}:{global_page}')
    g = global_page
    out = []
    for i in range(lines):
        if i % 2 == 0:
            words = [_english_word(rng) for _ in range(9)]
            if i == 0 and g % 5 == 0:
                words[3] = 'lithium'
            if i == 2 and g % 50 == 0:
                words[5] = 'dendrite'
            if i == 4 and g % 997 == 0:
                words[1] = 'zirconia'
            out.append(('F1', ' '.join(words)))
        else:
            parts = [_japanese_phrase(rng) for _ in range(4)]
            if i == 1 and g % 7 == 0:
                parts[1] += '電池'
            if i == 3 and g % 13 == 0:
                parts[2] += '電解質'
            if i == 5 and g % 97 == 0:
                parts[0] += '固体電解質'
            if i == 7 and g % 11 == 0:
                parts[3] += '電子'
            out.append(('F2', ''.join(parts)))
    return out


def corpus_pdf(seed, pdf_index, pages):
    """One corpus PDF and its pages' line texts (for the literal oracle)."""
    doc = Document(media_box=(0, 0, 595, 842))
    texts = []
    for p in range(pages):
        lines = page_lines(seed, pdf_index * pages + p)
        content = ''
        for i, (font, text) in enumerate(lines):
            content += text_line(font, 9, 50, 790 - 22 * i, text)
        doc.add_page(content)
        texts.append([text for _, text in lines])
    return doc.to_bytes(), texts


def write_corpus(directory, seed, pdfs, pages):
    """Writes `pdf-NNNN.pdf` files and returns the oracle: for every query term the sorted
    [pdf, page] pairs whose generated text contains it (literal, within one line)."""
    directory.mkdir(parents=True, exist_ok=False)
    hits = {term: [] for term in QUERY_TERMS}
    digest = hashlib.sha256()
    chars = 0
    total_bytes = 0
    for index in range(pdfs):
        data, texts = corpus_pdf(seed, index, pages)
        (directory / f'pdf-{index:04d}.pdf').write_bytes(data)
        digest.update(hashlib.sha256(data).digest())
        total_bytes += len(data)
        for page, lines in enumerate(texts):
            chars += sum(len(line) for line in lines)
            for term in QUERY_TERMS:
                if any(term in line for line in lines):
                    hits[term].append([index, page])
    return {
        'seed': seed, 'pdfs': pdfs, 'pages_per_pdf': pages, 'pages': pdfs * pages,
        'generated_chars': chars, 'pdf_bytes': total_bytes, 'corpus_sha256': digest.hexdigest(),
        'terms': {term: {'scalars': len(term), 'pages': len(pairs), 'hits': pairs}
                  for term, pairs in hits.items()},
    }
