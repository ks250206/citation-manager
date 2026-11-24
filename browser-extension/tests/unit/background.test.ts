// Background boundaries: who may open the capture port, what the content script may return,
// and how the session validates popup requests and host replies (with a fake native host).
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { checkSnapshot, isPopupSender } from '../../src/background/boundary';
import { HostClient, nativeError } from '../../src/background/host';
import { CaptureSession } from '../../src/background/session';
import { HOST_NAME, MiB } from '../../src/shared/limits';
import type { PageSnapshot } from '../../src/shared/types';
import { BoundaryError } from '../../src/shared/validate';
import { JOB, commitResult, err, job, ok, previewResult } from './samples';

const ID = 'baapgkfjfjjagdkpkmocjhfopmaohcdc';
const POPUP = `chrome-extension://${ID}/popup.html`;

test('only this extension’s popup document may open the capture port', () => {
  assert.equal(isPopupSender({ id: ID, url: POPUP }, ID, POPUP), true);
  assert.equal(isPopupSender({ id: ID, url: `${POPUP}?tab=7`, tab: { id: 8 }, frameId: 0 }, ID, POPUP), true, 'popup opened in a tab');
  assert.equal(isPopupSender({ id: ID, url: POPUP, origin: `chrome-extension://${ID}` }, ID, POPUP), true);
  // Firefox reports moz-extension URLs; same rule with its own popup URL.
  const ff = 'moz-extension://6f1f5a2e-24e2-4c1a-9a43-0000000000e2/popup.html';
  assert.equal(isPopupSender({ id: 'citation-manager-dev@citation-manager.invalid', url: ff }, 'citation-manager-dev@citation-manager.invalid', ff), true);

  assert.equal(isPopupSender(undefined, ID, POPUP), false);
  assert.equal(isPopupSender({ id: 'otherextensionidaaaaaaaaaaaaaaaa', url: POPUP }, ID, POPUP), false, 'other extension');
  assert.equal(isPopupSender({ id: ID, url: 'https://evil.example/popup.html', tab: { id: 3 }, frameId: 0 }, ID, POPUP), false, 'content script / web page');
  assert.equal(isPopupSender({ id: ID, url: `chrome-extension://${ID}/background.js` }, ID, POPUP), false, 'other extension page');
  assert.equal(isPopupSender({ id: ID, url: `${POPUP}.evil` }, ID, POPUP), false);
  assert.equal(isPopupSender({ id: ID, url: POPUP, frameId: 2, tab: { id: 3 } }, ID, POPUP), false, 'popup framed in a page');
  assert.equal(isPopupSender({ id: ID, url: POPUP, origin: 'https://evil.example' }, ID, POPUP), false);
  assert.equal(isPopupSender({ id: ID }, ID, POPUP), false);
});

const snapshot = (over: Partial<PageSnapshot> = {}): PageSnapshot => ({
  page_url: 'https://journal.example.org/a#sec',
  title: 'T',
  html: '<html></html>',
  meta: [{ name: 'citation_title', content: 'T' }],
  json_ld: [],
  pdf_url: null,
  content_type: 'text/html',
  html_truncated: false,
  dropped_meta: 0,
  dropped_json_ld: 0,
  ...over,
});

test('content script results are validated against the tab', () => {
  const tab = { id: 1, url: 'https://journal.example.org/a' };
  assert.equal(checkSnapshot([{ frameId: 0, result: snapshot() }], tab).title, 'T');
  const code = (fn: () => unknown) => {
    try {
      fn();
      return 'no error';
    } catch (error) {
      return (error as BoundaryError).code;
    }
  };
  assert.equal(code(() => checkSnapshot([{ frameId: 0, result: snapshot() }], { id: 1 })), 'unsupported_page', 'no activeTab grant: URL hidden');
  assert.equal(code(() => checkSnapshot([{ frameId: 0, result: snapshot() }], { id: 1, url: 'file:///home/u/a.html' })), 'unsupported_page');
  assert.equal(code(() => checkSnapshot([{ frameId: 3, result: snapshot() }], tab)), 'bad_snapshot', 'subframe result');
  assert.equal(code(() => checkSnapshot([], tab)), 'bad_snapshot');
  assert.equal(code(() => checkSnapshot([{ frameId: 0, result: 'x' }], tab)), 'bad_snapshot');
  assert.equal(code(() => checkSnapshot([{ frameId: 0, result: snapshot({ page_url: 'https://other.example/' }) }], tab)), 'bad_snapshot', 'navigated meanwhile');
  assert.equal(code(() => checkSnapshot([{ frameId: 0, result: snapshot({ meta: [{ name: 1 } as never] }) }], tab)), 'bad_snapshot');
  assert.equal(code(() => checkSnapshot([{ frameId: 0, result: snapshot({ pdf_url: 'https://elsewhere.example/x.pdf' }) }], tab)), 'bad_snapshot');
  assert.equal(code(() => checkSnapshot([{ frameId: 0, result: snapshot({ html: 5 as never }) }], tab)), 'bad_snapshot');
});

test('native messaging failures map to stable codes', () => {
  assert.equal(nativeError(new Error('Specified native messaging host not found.')).code, 'host_missing');
  assert.equal(nativeError(new Error('No such native application dev.citation_manager.native_host')).code, 'host_missing');
  assert.equal(nativeError(new Error('Access to the specified native messaging host is forbidden.')).code, 'host_forbidden');
  assert.equal(nativeError(new Error('Native host has exited.')).code, 'host_failed');
});

interface Sent {
  host: string;
  message: Record<string, unknown> & { id: string; type: string };
}

function fakeHost(answer: (m: Sent['message']) => unknown) {
  const sent: Sent[] = [];
  const client = new HostClient(async (host, message) => {
    const m = message as Sent['message'];
    sent.push({ host, message: m });
    const reply = answer(m);
    if (reply instanceof Error) throw reply;
    return reply;
  });
  return { sent, client };
}

const capture = (over: Record<string, unknown> = {}) => ({
  page_url: 'https://journal.example.org/a',
  title: 'T',
  html: '<html></html>',
  meta: [],
  json_ld: [],
  destination: { collection: null, tags: [] },
  offline: true,
  ...over,
});

function session(answer: (m: Sent['message']) => unknown) {
  const host = fakeHost(answer);
  const s = new CaptureSession({ host: host.client, extract: async () => snapshot() });
  return { ...host, s };
}

const previews = (id: string) => (m: Sent['message']) => {
  if (m.type === 'capture.preview') return ok(m, previewResult({ id }));
  if (m.type === 'capture.commit') return ok(m, commitResult());
  if (m.type === 'capture.cancel') return ok(m, { cancelled: true });
  if (m.type === 'capture.pdf_job') return ok(m, job('running'));
  return err(m, 'unknown_type');
};

test('requests carry v1 envelopes to the fixed host name', async () => {
  const { s, sent } = session(previews('p1'));
  const reply = await s.handle({ rid: 1, op: 'preview', capture: capture() });
  assert.equal(reply.rid, 1);
  assert.equal(reply.outcome.ok, true);
  assert.equal(sent[0]?.host, HOST_NAME);
  assert.equal(sent[0]?.message.v, 1);
  assert.equal(sent[0]?.message.type, 'capture.preview');
  assert.match(sent[0]?.message.id ?? '', /^[A-Za-z0-9._:-]{1,64}$/);
});

test('oversized or malformed popup requests never reach the host', async () => {
  const { s, sent } = session(previews('p1'));
  const big = await s.handle({ rid: 2, op: 'preview', capture: capture({ html: 'x'.repeat(2 * MiB + 1) }) });
  assert.deepEqual(big.outcome.ok ? null : big.outcome.error.code, 'too_large');
  const scheme = await s.handle({ rid: 3, op: 'preview', capture: capture({ page_url: 'file:///etc/passwd' }) });
  assert.equal(scheme.outcome.ok ? null : scheme.outcome.error.code, 'invalid_request');
  const unknown = await s.handle({ rid: 4, op: 'grants.issue' });
  assert.equal(unknown.outcome.ok ? null : unknown.outcome.error.code, 'bad_request');
  const junk = await s.handle('hello');
  assert.equal(junk.rid, -1);
  assert.equal(sent.length, 0);
});

test('commit indices are checked against this session’s own previews', async () => {
  const { s, sent } = session(previews('p1'));
  const unknown = await s.handle({ rid: 1, op: 'commit', preview_id: 'p1', decisions: [{ candidate_index: 0, action: 'Create' }], pdf: null });
  assert.equal(unknown.outcome.ok ? null : unknown.outcome.error.code, 'not_found', 'no such preview yet');
  await s.handle({ rid: 2, op: 'preview', capture: capture() });
  const badIndex = await s.handle({ rid: 3, op: 'commit', preview_id: 'p1', decisions: [{ candidate_index: 1, action: 'Create' }], pdf: null });
  assert.equal(badIndex.outcome.ok ? null : badIndex.outcome.error.code, 'invalid_request');
  const badPdf = await s.handle({ rid: 4, op: 'commit', preview_id: 'p1', decisions: [{ candidate_index: 0, action: 'Create' }], pdf: 1 });
  assert.equal(badPdf.outcome.ok ? null : badPdf.outcome.error.code, 'invalid_request');
  const merge = await s.handle({ rid: 5, op: 'commit', preview_id: 'p1', decisions: [{ candidate_index: 0, action: { Merge: { match_index: 0, fields: ['title'] } } }], pdf: null });
  assert.equal(merge.outcome.ok ? null : merge.outcome.error.code, 'invalid_request');
  assert.equal(sent.filter((m) => m.message.type === 'capture.commit').length, 0);
  const good = await s.handle({ rid: 6, op: 'commit', preview_id: 'p1', decisions: [{ candidate_index: 0, action: 'Create' }], pdf: 0 });
  assert.equal(good.outcome.ok, true);
  const commit = sent.find((m) => m.message.type === 'capture.commit')?.message;
  assert.deepEqual(commit?.decisions, [{ candidate_index: 0, action: 'Create' }]);
  assert.equal(commit?.pdf, 0);
  assert.deepEqual(s.openPreviews(), []);
});

test('superseded and abandoned previews are cancelled', async () => {
  let n = 0;
  const { s, sent } = session((m) => {
    if (m.type === 'capture.preview') return ok(m, previewResult({ id: `p${++n}` }));
    if (m.type === 'capture.cancel') return ok(m, { cancelled: true });
    return err(m, 'unknown_type');
  });
  await s.handle({ rid: 1, op: 'preview', capture: capture() });
  await s.handle({ rid: 2, op: 'preview', capture: capture() });
  assert.deepEqual(
    sent.filter((m) => m.message.type === 'capture.cancel').map((m) => m.message.preview_id),
    ['p1'],
  );
  await s.close();
  assert.deepEqual(
    sent.filter((m) => m.message.type === 'capture.cancel').map((m) => m.message.preview_id),
    ['p1', 'p2'],
  );
});

test('malformed or mismatched host replies become bad_reply', async () => {
  const cases: [string, (m: Sent['message']) => unknown][] = [
    ['wrong id', (m) => ({ ...ok(m, previewResult()), id: 'other' })],
    ['wrong type', (m) => ({ ...ok(m, previewResult()), type: 'capture.commit' })],
    ['wrong version', (m) => ({ ...ok(m, previewResult()), v: 2 })],
    ['not an object', () => 'ok'],
    ['bad result', (m) => ok(m, { preview: { id: 1 } })],
  ];
  for (const [name, answer] of cases) {
    const { s } = session(answer);
    const reply = await s.handle({ rid: 1, op: 'preview', capture: capture() });
    assert.equal(reply.outcome.ok ? null : reply.outcome.error.code, 'bad_reply', name);
    assert.deepEqual(s.openPreviews(), [], name);
  }
});

test('host errors and a missing host pass through with their codes', async () => {
  const unavailable = session((m) => err(m, 'daemon_unavailable', true));
  const reply = await unavailable.s.handle({ rid: 1, op: 'pdf_job', job: JOB });
  assert.deepEqual(reply.outcome, { ok: false, error: { code: 'daemon_unavailable', message: 'daemon_unavailable message', retryable: true } });
  const missing = session(() => new Error('Specified native messaging host not found.'));
  const r2 = await missing.s.handle({ rid: 2, op: 'hello' });
  assert.equal(r2.outcome.ok ? null : r2.outcome.error.code, 'host_missing');
  const badJob = session(previews('p'));
  const r3 = await badJob.s.handle({ rid: 3, op: 'retry_pdf', job: 'not-a-uuid' });
  assert.equal(r3.outcome.ok ? null : r3.outcome.error.code, 'bad_request');
  assert.equal(badJob.sent.length, 0);
});
