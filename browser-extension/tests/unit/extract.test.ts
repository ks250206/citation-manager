// HTML fixture regression for the page extractor (fixtures/html from #22) and its bounds.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { extractSnapshot } from '../../src/content/extract';
import { MAX_CAPTURE_BYTES, MAX_HTML_BYTES, MAX_JSON_LD_BLOCKS, MAX_META_TAGS } from '../../src/shared/limits';
import { buildCaptureRequest } from '../../src/shared/request';
import { utf8Length } from '../../src/shared/utf8';
import { checkCaptureRequest } from '../../src/shared/validate';
import { dom, fixture, fixtureNames } from './helpers';

const PAGE = 'https://journal.example.org/articles/1';
const metaValue = (s: ReturnType<typeof extractSnapshot>, name: string) => s.meta.filter((m) => m.name.toLowerCase() === name).map((m) => m.content);

test('every #22 HTML fixture yields a request the host contract accepts', () => {
  const names = fixtureNames();
  assert.ok(names.length >= 7, `fixtures found: ${names}`);
  for (const name of names) {
    const snapshot = extractSnapshot(dom(fixture(name)), PAGE);
    const request = checkCaptureRequest(buildCaptureRequest(snapshot, { collection: null, tags: [] }, true));
    assert.equal(request.page_url, PAGE, name);
    assert.ok(snapshot.html !== null, `${name}: html`);
    // <meta> and every <script> are sent structured or not at all, never inside the HTML.
    // (HTML comments are inert text and may mention tags.)
    assert.doesNotMatch((snapshot.html ?? '').replace(/<!--[\s\S]*?-->/g, ''), /<meta\b|<script\b|<style\b/i, name);
  }
});

test('publisher.html: Highwire, Dublin Core, JSON-LD and the PDF link survive', () => {
  const s = extractSnapshot(dom(fixture('publisher.html')), PAGE);
  assert.equal(s.title, 'Synthetic Journal | Lattice Methods for Sparse Citation Graphs');
  assert.deepEqual(metaValue(s, 'citation_doi'), ['10.9999/jse.2024.0012']);
  assert.deepEqual(metaValue(s, 'citation_author'), ['Schröder, Jörg', 'García, José', 'Example Consortium']);
  assert.deepEqual(metaValue(s, 'citation_pdf_url'), ['/content/jse.2024.0012.pdf']);
  assert.deepEqual(metaValue(s, 'og:title'), ['Lattice Methods — Synthetic Journal']);
  // Entities are decoded by the DOM; the value stays data.
  assert.deepEqual(metaValue(s, 'citation_abstract'), ['We describe lattice methods & their <bounded> behaviour.']);
  assert.equal(s.json_ld.length, 1);
  assert.match(s.json_ld[0] ?? '', /ScholarlyArticle/);
  assert.match(s.html ?? '', /<link rel="alternate" type="application\/pdf" href="https:\/\/cdn\.example\.org\/jse\/0012\.pdf">/);
  // A normal page: the daemon finds PDF candidates itself; no explicit choice is sent.
  assert.equal(s.pdf_url, null);
  assert.equal(s.html_truncated, false);
});

test('arxiv, pubmed, jsonld and dublin-core fixtures keep their identifying tags', () => {
  const arxiv = extractSnapshot(dom(fixture('arxiv.html')), 'https://arxiv.org/abs/2401.01234v2');
  assert.ok(metaValue(arxiv, 'citation_arxiv_id').length === 1);
  const pubmed = extractSnapshot(dom(fixture('pubmed.html')), 'https://pubmed.ncbi.nlm.nih.gov/123/');
  assert.ok(metaValue(pubmed, 'citation_pmid').length === 1);
  assert.ok(metaValue(pubmed, 'citation_authors').length === 1);
  const jsonld = extractSnapshot(dom(fixture('jsonld.html')), PAGE);
  assert.ok(jsonld.json_ld.length >= 2, 'graph block and the malformed block are both forwarded as data');
  const dc = extractSnapshot(dom(fixture('dublin-core.html')), PAGE);
  assert.ok(dc.meta.some((m) => m.name.toLowerCase().startsWith('dc.')));
});

test('no-metadata.html: only the title and the page text', () => {
  const s = extractSnapshot(dom(fixture('no-metadata.html')), PAGE);
  assert.ok(s.title);
  assert.equal(s.meta.filter((m) => m.name.startsWith('citation_')).length, 0);
  assert.equal(s.json_ld.length, 0);
});

test('malicious.html: strings stay data, comments and scripts are not evaluated', () => {
  const s = extractSnapshot(dom(fixture('malicious.html')), PAGE);
  // The command-looking title is kept verbatim as text for the daemon to treat as data.
  assert.match(s.title ?? '', /rm -rf/);
  // Commented-out meta is not a meta element; script-written meta never existed (no script ran).
  assert.ok(!s.meta.some((m) => m.content === 'Commented-out title must be ignored'));
  assert.ok(!s.meta.some((m) => m.content === 'Injected by script'));
  // Spoofed authority meta is forwarded as plain page data; the daemon ignores it.
  assert.deepEqual(metaValue(s, 'client_kind'), ['desktop']);
  assert.doesNotMatch(s.html ?? '', /<script|document\.write|<style/i);
  assert.ok(checkCaptureRequest(buildCaptureRequest(s, { collection: null, tags: [] }, true)));
});

test('a huge page is cut at a UTF-8 boundary within the HTML and total limits', () => {
  const body = 'é漢🙂'.repeat(400_000); // ~3.6 MB of multi-byte text
  const s = extractSnapshot(dom(`<html><head><title>Big</title></head><body><p>${body}</p></body></html>`), PAGE);
  assert.equal(s.html_truncated, true);
  const bytes = utf8Length(s.html ?? '');
  assert.ok(bytes <= MAX_HTML_BYTES, `${bytes}`);
  assert.ok(bytes > MAX_HTML_BYTES - 8, 'uses the budget');
  assert.ok(!(s.html ?? '').includes('�'), 'no split code point');
  checkCaptureRequest(buildCaptureRequest(s, { collection: null, tags: [] }, false));
});

test('meta tags and JSON-LD over the limits are dropped whole, never cut', () => {
  const big = 'x'.repeat(70 * 1024);
  const many = Array.from({ length: MAX_META_TAGS + 10 }, (_, i) => `<meta name="m${i}" content="v">`).join('');
  const ld = Array.from({ length: MAX_JSON_LD_BLOCKS + 3 }, (_, i) => `<script type="application/ld+json">{"i":${i}}</script>`).join('');
  const s = extractSnapshot(dom(`<html><head><meta name="citation_title" content="${big}">${many}${ld}</head><body></body></html>`), PAGE);
  assert.ok(!s.meta.some((m) => m.name === 'citation_title'));
  assert.equal(s.meta.length, MAX_META_TAGS);
  assert.equal(s.dropped_meta, 11);
  assert.equal(s.json_ld.length, MAX_JSON_LD_BLOCKS);
  assert.equal(s.dropped_json_ld, 3);
  const request = checkCaptureRequest(buildCaptureRequest(s, { collection: null, tags: [] }, true));
  const total = utf8Length(request.html ?? '') + request.meta.reduce((n, m) => n + utf8Length(m.name + m.content), 0);
  assert.ok(total <= MAX_CAPTURE_BYTES);
});

test('the total budget shrinks the HTML when JSON-LD is large', () => {
  const block = `{"a":"${'y'.repeat(500 * 1024)}"}`;
  const ld = Array.from({ length: 3 }, () => `<script type="application/ld+json">${block}</script>`).join('');
  const s = extractSnapshot(dom(`<html><head>${ld}</head><body>${'z'.repeat(3 * 1024 * 1024)}</body></html>`), PAGE, {
    html: MAX_HTML_BYTES,
    meta: 512 * 1024,
    jsonLd: 4 * 1024 * 1024,
  });
  assert.equal(s.json_ld.length, 3);
  const request = checkCaptureRequest(buildCaptureRequest(s, { collection: null, tags: [] }, true));
  const total = utf8Length(request.html ?? '') + request.json_ld.reduce((n, b) => n + utf8Length(b), 0);
  assert.ok(total <= MAX_CAPTURE_BYTES - 64 * 1024, `${total}`);
});

test('lone surrogates become U+FFFD so the JSON is valid UTF-8', () => {
  const s = extractSnapshot(dom('<html><head><title>a\uD800b</title><meta name="citation_title" content="x\uDC00y"></head></html>'), PAGE);
  assert.equal(s.title, 'a�b');
  assert.equal(metaValue(s, 'citation_title')[0], 'x�y');
});

test('a PDF document in the tab is the explicit PDF choice', () => {
  const doc = dom('<html><body><embed type="application/pdf"></body></html>');
  Object.defineProperty(doc, 'contentType', { value: 'application/pdf' });
  const s = extractSnapshot(doc, 'https://cdn.example.org/paper.pdf');
  assert.equal(s.pdf_url, 'https://cdn.example.org/paper.pdf');
  assert.equal(s.html, null);
});
