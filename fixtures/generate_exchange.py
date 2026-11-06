"""Deterministic, synthetic original-format fixtures (not serializer outputs)."""
from hashlib import sha256
from pathlib import Path
ROOT = Path(__file__).resolve().parent
bib, ris = [], []
for i in range(1000):
    title = f"研究 Ω {sha256(f'paper-{i}'.encode()).hexdigest()}"
    kind = ['article', 'book', 'inproceedings', 'incollection', 'phdthesis', 'techreport', 'misc'][i % 7]
    typ = ['JOUR', 'BOOK', 'CONF', 'CHAP', 'THES', 'RPRT', 'GEN'][i % 7]
    year = 2000 + i % 26
    bib.append(f'''@{kind}{{Fixture{i},
  title = {{{title}: A \\& B \\{{literal\\}}}},
  author = {{García, José and {{研究 and Development Consortium}} and 山田, 花子}},
  editor = {{Curie, Marie}},
  year = {{{year}}},
  date = {{{year}-02-03}},
  journal = {{誌名 & Journal}},
  volume = {{{i % 40 + 1}}},
  number = {{{i % 12 + 1}}},
  pages = {{S1--S12}},
  doi = {{https://doi.org/10.9999/FIXTURE.{i}}},
  url = {{https://example.invalid/paper/{i}?a=1&b=2}},
  abstract = {{Résumé Ω 日本語; 50\\% \\#tag and \\textbackslash{{}}path}},
  keywords = {{固体電解質; Unicode Ω}},
  urldate = {{2026-11-07}},
  language = {{ja}},
  publisher = {{Synthetic Press {i}}},
  customfield = {{unknown Ω {i}}}
}}
''')
    ris.append(f'''TY  - {typ}
ID  - Fixture{i}
TI  - {title}: A & B {{literal}}
AU  - García, José
AU  - {{研究 and Development Consortium}}
AU  - 山田, 花子
ED  - Curie, Marie
PY  - {year}
DA  - {year}/02/03/
JO  - 誌名 & Journal
VL  - {i % 40 + 1}
IS  - {i % 12 + 1}
SP  - S1
EP  - S12
DO  - https://doi.org/10.9999/FIXTURE.{i}
UR  - https://example.invalid/paper/{i}?a=1&b=2
AB  - Résumé Ω 日本語; 50% #tag and \\path
  second line; Ω
KW  - 固体電解質
KW  - Unicode Ω
Y2  - 2026-11-07
LA  - ja
PB  - Synthetic Press {i}
ZZ  - unknown Ω {i}
ER  -

''')
(ROOT / 'bibtex' / 'thousand.bib').write_text('\n'.join(bib))
(ROOT / 'ris' / 'thousand.ris').write_text(''.join(ris).rstrip() + '\n')
