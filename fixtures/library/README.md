Original tiny synthetic PDFs, dedicated to the public domain under CC0-1.0
([full text](../pdf/CC0-1.0.txt)). Regenerate with Python's standard library:
`python3 scripts/generate_library_fixtures.py`. Hashes are in manifest.json.

- `metadata.pdf`: valid document with uncompressed catalog-associated XMP and Info
  dictionary. Conflicting DOI/title are deliberate, untrusted suggestions for review.
- `info.pdf`: valid document using only the real Info dictionary.
- `scanned.pdf`: one image-only page, no extractable text or bibliographic title.

Existing `fixtures/pdf/{encrypted,malformed,corpus}.pdf` provide password/corruption,
Unicode and an image-only page. Copy a PDF to a second fresh path for identical-hash
selection. None contains user documents or third-party scientific content.
