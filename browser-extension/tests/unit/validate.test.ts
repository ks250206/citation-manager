// Boundary tests: outgoing size/shape limits and incoming host reply validation.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { MiB } from '../../src/shared/limits';
import { truncateUtf8, utf8Length, wellFormed } from '../../src/shared/utf8';
import {
  BoundaryError,
  checkCaptureRequest,
  checkDecisions,
  checkReply,
  parseCommit,
  parseDestinations,
  parseHello,
  parseJob,
  parsePreview,
} from '../../src/shared/validate';
import { COLLECTION, DESTINATIONS, HELLO, REDACTED_WARNING, commitResult, job, previewResult } from './samples';

const base = () => ({
  page_url: 'https://journal.example.org/a',
  title: 'T',
  html: '<html></html>',
  meta: [{ name: 'citation_title', content: 'T' }],
  json_ld: [],
  destination: { collection: null, tags: [] },
  offline: false,
});

const code = (fn: () => unknown) => {
  try {
    fn();
  } catch (error) {
    assert.ok(error instanceof BoundaryError, String(error));
    return error.code;
  }
  return 'no error';
};

test('utf8 truncation never splits a code point', () => {
  assert.deepEqual(truncateUtf8('abc', 10), { text: 'abc', truncated: false });
  assert.equal(truncateUtf8('aé', 2).text, 'a'); // é is 2 bytes
  assert.equal(truncateUtf8('a🙂', 4).text, 'a'); // 🙂 is 4 bytes
  assert.equal(truncateUtf8('a🙂', 5).text, 'a🙂');
  assert.equal(truncateUtf8('漢字', 5).text, '漢');
  assert.equal(utf8Length('é漢🙂'), 9);
  assert.equal(wellFormed('a\uD83Db'), 'a�b');
  assert.equal(wellFormed('🙂'), '🙂');
});

test('a valid capture request passes unchanged', () => {
  const request = checkCaptureRequest({ ...base(), pdf_url: 'https://cdn.example.org/a.pdf', destination: { collection: COLLECTION, tags: [] }, client_kind: 'desktop' });
  assert.equal(request.pdf_url, 'https://cdn.example.org/a.pdf');
  assert.equal(request.destination.collection, COLLECTION);
  // Unknown (page-forwarded) fields are not passed on.
  assert.ok(!('client_kind' in request));
});

test('oversized captures are refused before reaching the host', () => {
  assert.equal(code(() => checkCaptureRequest({ ...base(), html: 'x'.repeat(2 * MiB + 1) })), 'too_large');
  assert.equal(code(() => checkCaptureRequest({ ...base(), html: 'é'.repeat(MiB + 1) })), 'too_large', 'bytes, not UTF-16 units');
  assert.equal(code(() => checkCaptureRequest({ ...base(), title: 'x'.repeat(16 * 1024 + 1) })), 'too_large');
  assert.equal(code(() => checkCaptureRequest({ ...base(), page_url: `https://a.example/${'x'.repeat(4096)}` })), 'too_large');
  assert.equal(code(() => checkCaptureRequest({ ...base(), meta: Array.from({ length: 1025 }, () => ({ name: 'a', content: 'b' })) })), 'too_large');
  assert.equal(code(() => checkCaptureRequest({ ...base(), meta: [{ name: 'n'.repeat(257), content: '' }] })), 'too_large');
  assert.equal(code(() => checkCaptureRequest({ ...base(), meta: [{ name: 'n', content: 'c'.repeat(64 * 1024 + 1) }] })), 'too_large');
  assert.equal(code(() => checkCaptureRequest({ ...base(), json_ld: Array.from({ length: 33 }, () => '{}') })), 'too_large');
  assert.equal(code(() => checkCaptureRequest({ ...base(), json_ld: ['x'.repeat(512 * 1024 + 1)] })), 'too_large');
  // Each part within its own limit, the total over 4 MiB.
  const block = 'j'.repeat(500 * 1024);
  assert.equal(code(() => checkCaptureRequest({ ...base(), html: 'h'.repeat(2 * MiB), json_ld: Array.from({ length: 5 }, () => block) })), 'too_large');
});

test('malformed captures are refused', () => {
  assert.equal(code(() => checkCaptureRequest(null)), 'invalid_request');
  assert.equal(code(() => checkCaptureRequest({ ...base(), page_url: 'file:///etc/passwd' })), 'invalid_request');
  assert.equal(code(() => checkCaptureRequest({ ...base(), page_url: 'chrome://settings' })), 'invalid_request');
  assert.equal(code(() => checkCaptureRequest({ ...base(), pdf_url: 'javascript:alert(1)' })), 'invalid_request');
  assert.equal(code(() => checkCaptureRequest({ ...base(), meta: [{ name: 1, content: 'x' }] })), 'invalid_request');
  assert.equal(code(() => checkCaptureRequest({ ...base(), offline: 'yes' })), 'invalid_request');
  assert.equal(code(() => checkCaptureRequest({ ...base(), destination: { collection: '../../etc', tags: [] } })), 'invalid_request');
  assert.equal(code(() => checkCaptureRequest({ ...base(), destination: { collection: null, tags: ['x'] } })), 'invalid_request');
});

test('decisions are Create/Skip over known candidates only', () => {
  assert.deepEqual(checkDecisions([{ candidate_index: 0, action: 'Create' }], 1), [{ candidate_index: 0, action: 'Create' }]);
  assert.equal(code(() => checkDecisions([], 1)), 'invalid_request');
  assert.equal(code(() => checkDecisions([{ candidate_index: 1, action: 'Create' }], 1)), 'invalid_request');
  assert.equal(code(() => checkDecisions([{ candidate_index: 0, action: { Merge: { match_index: 0, fields: [] } } }], 1)), 'invalid_request');
  assert.equal(code(() => checkDecisions(Array.from({ length: 17 }, () => ({ candidate_index: 0, action: 'Skip' })), 1)), 'invalid_request');
});

test('reply envelopes must echo our id, version and type', () => {
  const parse = parseHello;
  assert.deepEqual(checkReply({ v: 1, id: 'a', ok: true, type: 'hello', result: HELLO }, 'a', 'hello', parse).ok, true);
  assert.equal(code(() => checkReply({ v: 1, id: 'b', ok: true, type: 'hello', result: HELLO }, 'a', 'hello', parse)), 'bad_reply');
  assert.equal(code(() => checkReply({ v: 2, id: 'a', ok: true, type: 'hello', result: HELLO }, 'a', 'hello', parse)), 'bad_reply');
  assert.equal(code(() => checkReply({ v: 1, id: 'a', ok: true, type: 'destinations', result: HELLO }, 'a', 'hello', parse)), 'bad_reply');
  assert.equal(code(() => checkReply({ v: 1, id: 'a', type: 'hello', result: HELLO }, 'a', 'hello', parse)), 'bad_reply');
  assert.equal(code(() => checkReply('{"v":1}', 'a', 'hello', parse)), 'bad_reply');
  assert.equal(code(() => checkReply(null, 'a', 'hello', parse)), 'bad_reply');
  assert.equal(code(() => checkReply({ v: 1, id: 'a', ok: true, type: 'hello', result: { ...HELLO, paired: 'yes' } }, 'a', 'hello', parse)), 'bad_reply');
});

test('host errors keep code and retryable flag; malformed errors are bad replies', () => {
  const outcome = checkReply({ v: 1, id: 'a', ok: false, error: { code: 'daemon_unavailable', message: 'down', retryable: true } }, 'a', 'hello', parseHello);
  assert.deepEqual(outcome, { ok: false, error: { code: 'daemon_unavailable', message: 'down', retryable: true } });
  // The host answers id null when it could not read the request id.
  assert.equal(checkReply({ v: 1, id: null, ok: false, error: { code: 'bad_message', message: 'x', retryable: false } }, 'a', 'hello', parseHello).ok, false);
  assert.equal(code(() => checkReply({ v: 1, id: 'a', ok: false, error: { code: 3 } }, 'a', 'hello', parseHello)), 'bad_reply');
  assert.equal(code(() => checkReply({ v: 1, id: 'a', ok: false, error: { code: 'x', message: 'm'.repeat(5000), retryable: false } }, 'a', 'hello', parseHello)), 'bad_reply');
});

test('preview results are read from the cm-protocol shapes', () => {
  const p = parsePreview(previewResult({ warnings: [REDACTED_WARNING], hiddenDuplicates: 1 }));
  assert.equal(p.preview_id, 'preview-1');
  assert.equal(p.candidates[0]?.draft.title, 'Capturing Papers');
  assert.equal(p.candidates[0]?.draft.year, 2026);
  assert.equal(p.candidates[0]?.draft.authors[0]?.family_name, 'Lovelace');
  assert.equal(p.hidden_duplicates, 1);
  // The only row is the hidden duplicate: it is treated like a visible exact duplicate.
  assert.equal(p.candidates[0]?.exact_duplicate, true);
  assert.deepEqual(p.candidates[0]?.matches, []);
  assert.deepEqual(p.pdf_candidates, [{ url: 'https://cdn.example.org/a.pdf', origin: 'highwire' }]);
  const existing = { id: 'x', title: 'Old', external_ids: [], doi: '10.9999/x', year: 2020 };
  const dup = parsePreview(previewResult({ matches: [{ existing, kind: 'Doi', differences: [] }] }));
  assert.equal(dup.candidates[0]?.exact_duplicate, true);
  assert.equal(dup.candidates[0]?.matches[0]?.title, 'Old');
  const fuzzy = parsePreview(previewResult({ matches: [{ existing, kind: 'SimilarTitle', differences: [] }] }));
  assert.equal(fuzzy.candidates[0]?.exact_duplicate, false);
});

test('hidden duplicates come from the structured field, never from warning text', () => {
  // Absent (older daemon): none. The warning text, old or new, is not parsed.
  for (const warnings of [[], [REDACTED_WARNING], ['1 possible existing-library matches hidden for this client']]) {
    const p = parsePreview(previewResult({ warnings }));
    assert.equal(p.hidden_duplicates, 0);
    assert.equal(p.candidates[0]?.exact_duplicate, false);
  }
  assert.equal(parsePreview(previewResult({ hiddenDuplicates: 0 })).hidden_duplicates, 0);
  // Some rows of several: counted, but no row can be singled out.
  const some = parsePreview(previewResult({ rows: 3, hiddenDuplicates: 2 }));
  assert.equal(some.hidden_duplicates, 2);
  assert.deepEqual(some.candidates.map((c) => c.exact_duplicate), [false, false, false]);
  const all = parsePreview(previewResult({ rows: 2, hiddenDuplicates: 2 }));
  assert.deepEqual(all.candidates.map((c) => c.exact_duplicate), [true, true]);
  // More than the rows, negative, fractional or not a number: a bad reply.
  for (const bad of [2, -1, 0.5, '1']) {
    const raw = { ...previewResult(), hidden_duplicates: bad };
    assert.equal(code(() => parsePreview(raw)), 'bad_reply', String(bad));
  }
});

test('malformed previews are rejected', () => {
  assert.equal(code(() => parsePreview({})), 'bad_reply');
  assert.equal(code(() => parsePreview(previewResult({ pdfs: [{ url: 'javascript:alert(1)', origin: 'highwire' }] }))), 'bad_reply');
  assert.equal(code(() => parsePreview(previewResult({ pdfs: Array.from({ length: 17 }, () => ({ url: 'https://a.example/x.pdf', origin: 'link' })) }))), 'bad_reply');
  const r = previewResult();
  (r.preview.candidates as unknown[]) = [];
  assert.equal(code(() => parsePreview(r)), 'bad_reply');
  const t = previewResult();
  (t.preview.candidates[0] as { draft: { title: unknown } }).draft.title = 42;
  assert.equal(code(() => parsePreview(t)), 'bad_reply');
});

test('commit, job and destination results', () => {
  const c = parseCommit(commitResult());
  assert.equal(c.created, 1);
  assert.equal(c.pdf_job?.state, 'queued');
  assert.equal(parseCommit({ ...commitResult(null), paper: null }).paper, null);
  assert.equal(code(() => parseCommit({ ...commitResult(), paper: '../x' })), 'bad_reply');
  assert.equal(parseJob(job('retry_wait', 2, 'network error')).error, 'network error');
  assert.equal(code(() => parseJob(job('exploded'))), 'bad_reply');
  assert.equal(code(() => parseJob({ ...job('failed'), id: 'nope' })), 'bad_reply');
  const d = parseDestinations(DESTINATIONS);
  assert.equal(d.collections.length, 2);
  assert.equal(d.tags[0]?.name, 'to-read');
  assert.equal(code(() => parseDestinations({ collections: [{ id: 'x', parent: null, name: 'a', sort_order: 0 }], tags: [] })), 'bad_reply');
});
