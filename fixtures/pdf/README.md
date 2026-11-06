# Synthetic PDF corpus (#11)

All prose, vector shapes and generated image pixels are original test content created for this
repository, dedicated to CC0-1.0 (see `CC0-1.0.txt`). No papers, downloads of papers, or private data.
`corpus.pdf` has six pages: four identical Unicode/multiline pages with rotations 0/90/180/270,
an embedded blue image with no text, and a page with text close to both CropBox boundaries.
MediaBox is [0,0,400,500], CropBox [20,30,380,470]. Red marker [40,50,52,58] is a physical reference.
Labels `iv`/`S12` deliberately differ from the 0-based index and 1-based physical display number.

`inherited.pdf` is a separate two-page original derivative of the Unicode/marker content.
Its `/Pages` dictionary owns MediaBox [0,0,400,500], CropBox [20,30,380,470], and Rotate 90.
Page 0 omits all three; page 1 has only a direct MediaBox and inherits CropBox/Rotate.
Both must render at 440×360 at zoom 1. The generator removes entries only from writer-owned
raw dictionaries after adding pages; `inherited_boxes.py` checks `/Root/Pages/Kids` without
accessing `PdfReader.pages`, which could silently flatten inheritance. Negative tests reject
flattened child dictionaries and a changed parent crop. The inherited export uses fixed 20pt-high
PDF-space rectangles [44,406,330,426] / [44,378,330,398], independent of extracted glyph geometry.
All derivative content has the same CC0/OFL permissions as `corpus.pdf`.

`encrypted.pdf` is an original one-page derivative, user password `test-password`, owner
`owner-password`, RC4-128 solely to exercise the legacy password error/success paths.
It is not an encryption/security recommendation. `malformed.pdf` is a deliberately incomplete catalog.
`manifest.json` records exact committed PDF hashes and expected content/coordinates.

The PDF embeds a subset of Noto Sans JP (SIL Open Font License 1.1, `OFL-NotoSansJP.txt`).
The font is from google/fonts commit `66a36c8c94b1a5d992ee4e7f392fccfe4945767c`,
`ofl/notosansjp/NotoSansJP[wght].ttf`; exact URL/hash are in `scripts/pdfium/runtime-lock.json`.
The OFL permits embedding/redistribution; the generated documents are not placed under the OFL.
Font license/attribution accompanies the fixtures. No proprietary/system font is copied.
See [OFL embedding FAQ](https://openfontlicense.org/ofl-faq/) and the included license for terms.
The variable font is instantiated at wght=400 by fontTools and subset-embedded by ReportLab;
no standalone modified font is redistributed.

Regeneration (use a fresh output directory to compare against committed fixtures):

```sh
uv venv target/pdfium-tools
uv pip install --python target/pdfium-tools/bin/python -r scripts/pdfium/requirements.txt
# Download font_url from runtime-lock.json and verify font_sha256 before use.
target/pdfium-tools/bin/python scripts/pdfium/generate_fixtures.py \
  --font target/pdfium-poc/NotoSansJP.ttf --output target/regenerated-pdf
```

Generator/tool versions are pinned in `scripts/pdfium/requirements.txt`. Fixture hashes are the
identity for runtime regression. Export timestamps/IDs can vary between PoC runs; tests compare
page/content/geometry/appearances and preservation of the original, not exported file identity.
