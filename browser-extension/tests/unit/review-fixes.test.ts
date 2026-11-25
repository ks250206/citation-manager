// Review fixes: popup port deadlines/reconnect/ping, Back after a failed re-preview,
// corrections (clearing, container type, rejected DOI), too_large retry, hints, IDs.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildManifest, checkIds, type ExtensionIds } from '../../src/manifest';
import { Controller, shrinkSnapshot, type Client } from '../../src/popup/controller';
import { COMMIT_UNKNOWN_MESSAGE, portClient } from '../../src/popup/port-client';
import { canCommit, initialState, reduce } from '../../src/popup/state';
import { correctionNotes, render, snapshotHints } from '../../src/popup/view';
import type { Port } from '../../src/shared/api';
import { buildCaptureRequest, clearedFields, normalizeCorrections } from '../../src/shared/request';
import type { Outcome, PageSnapshot } from '../../src/shared/types';
import { utf8Length } from '../../src/shared/utf8';
import { parseDestinations, parseHello, parsePreview } from '../../src/shared/validate';
import { dom } from './helpers';
import { CHILD, COLLECTION, DESTINATIONS, HELLO, previewResult } from './samples';

const SNAPSHOT: PageSnapshot = {
  page_url: 'https://proceedings.example.org/a',
  title: 'Doc',
  html: `<html><body>${'é'.repeat(1000)}</body></html>`,
  meta: [
    { name: 'citation_title', content: 'Page title' },
    { name: 'citation_conference_title', content: 'Proc. of Fixtures' },
    { name: 'citation_doi', content: '10.9999/page' },
  ],
  json_ld: [],
  pdf_url: null,
  content_type: 'text/html',
  html_truncated: false,
  dropped_meta: 3,
  dropped_json_ld: 1,
};
const okOf = <T>(result: T): Outcome<T> => ({ ok: true, result });

// ---- port client -------------------------------------------------------------------------

class FakePort implements Port {
  name = 'capture-popup';
  sent: Record<string, unknown>[] = [];
  private messageListeners: ((m: unknown, p: Port) => void)[] = [];
  private disconnectListeners: ((p: Port) => void)[] = [];
  onMessage = { addListener: (f: (m: unknown, p: Port) => void) => this.messageListeners.push(f), removeListener: () => undefined };
  onDisconnect = { addListener: (f: (p: Port) => void) => this.disconnectListeners.push(f), removeListener: () => undefined };
  postMessage(m: unknown) {
    this.sent.push(m as Record<string, unknown>);
  }
  disconnect() {}
  reply(rid: number, outcome: Outcome<unknown>) {
    for (const f of this.messageListeners) f({ rid, outcome }, this);
  }
  drop() {
    for (const f of this.disconnectListeners) f(this);
  }
}

function harnessPort() {
  const ports: FakePort[] = [];
  const timers: (() => void)[] = [];
  let ping: (() => void) | null = null;
  const client = portClient({
    connect: () => {
      const p = new FakePort();
      ports.push(p);
      return p;
    },
    setTimer: (fn) => timers.push(fn),
    clearTimer: () => undefined,
    setRepeat: (fn) => (ping = fn),
  });
  return { client, ports, timers, firePing: () => ping?.() };
}

test('port client: replies are matched by rid; a deadline gives a retryable timeout', async () => {
  const h = harnessPort();
  const hello = h.client.request({ op: 'hello' });
  const rid = h.ports[0]?.sent[0]?.rid as number;
  h.ports[0]?.reply(rid, okOf('hi'));
  assert.deepEqual(await hello, okOf('hi'));
  const slow = h.client.request({ op: 'preview', capture: {} as never });
  h.timers.at(-1)?.();
  const out = await slow;
  assert.deepEqual(out.ok ? null : [out.error.code, out.error.retryable], ['timeout', true]);
});

test('port client: a lost worker fails pending requests and the next request reconnects', async () => {
  const h = harnessPort();
  const pending = h.client.request({ op: 'destinations' });
  const commit = h.client.request({ op: 'commit', preview_id: 'p', decisions: [], pdf: null });
  h.ports[0]?.drop();
  const a = await pending;
  assert.deepEqual(a.ok ? null : [a.error.code, a.error.retryable], ['background_gone', true]);
  const c = await commit;
  assert.deepEqual(c.ok ? null : c.error, { code: 'commit_unknown', message: COMMIT_UNKNOWN_MESSAGE, retryable: false });
  const again = h.client.request({ op: 'hello' });
  assert.equal(h.ports.length, 2, 'reconnected');
  h.ports[1]?.reply(h.ports[1]?.sent[0]?.rid as number, okOf(1));
  assert.equal((await again).ok, true);
});

test('port client: pings the background while open', () => {
  const h = harnessPort();
  h.firePing();
  assert.equal(h.ports[0]?.sent[0]?.op, 'ping');
  h.client.close();
});

// ---- popup flow --------------------------------------------------------------------------

function controller(script: Record<string, (body: Record<string, unknown>) => Outcome<unknown>>) {
  const calls: Record<string, unknown>[] = [];
  const client: Client = {
    request: async (body) => {
      calls.push(body as Record<string, unknown>);
      const f = script[body.op];
      if (!f) throw new Error(body.op);
      return f(body as Record<string, unknown>);
    },
  };
  const ctl = new Controller({ client, tabId: async () => 1, sleep: async () => undefined, remember: () => undefined });
  return { ctl, calls };
}

const base = () => ({
  hello: () => okOf(parseHello(HELLO)),
  destinations: () => okOf(parseDestinations(DESTINATIONS)),
  extract: () => okOf(SNAPSHOT),
  preview: () => okOf(parsePreview(previewResult())),
});

test('Back after a failed re-preview restores the previewed destination; commit is enabled', async () => {
  let previews = 0;
  const { ctl } = controller({
    ...base(),
    preview: () => (++previews === 1 ? okOf(parsePreview(previewResult())) : { ok: false, error: { code: 'invalid_input', message: 'unknown collection', retryable: false } }),
  });
  await ctl.start();
  await ctl.setCollection(COLLECTION); // before the preview
  await ctl.save();
  await ctl.setCollection(CHILD); // re-preview fails
  assert.equal(ctl.state.phase.kind as string, 'error');
  ctl.dispatch({ type: 'back' });
  assert.equal(ctl.state.phase.kind as string, 'review');
  assert.equal(ctl.state.destination.collection, COLLECTION);
  assert.equal(canCommit(ctl.state), true);
});

test('an unknown commit outcome drops the preview: no blind re-save', () => {
  let s = reduce(initialState(), { type: 'extracted', outcome: okOf(SNAPSHOT) });
  s = reduce(s, { type: 'previewed', outcome: okOf(parsePreview(previewResult())), destination: { collection: null, tags: [] }, corrections: {} });
  s = reduce(s, { type: 'committed', outcome: { ok: false, error: { code: 'commit_unknown', message: 'check the app', retryable: false } } });
  assert.equal(s.phase.kind === 'error' && s.phase.recovery, 'none');
  assert.equal(s.preview, null);
  s = reduce(s, { type: 'back' });
  assert.equal(s.phase.kind, 'ready');
});

test('too_large: the HTML is cut by a quarter and the preview retried once', async () => {
  const sizes: number[] = [];
  const { ctl } = controller({
    ...base(),
    preview: (body) => {
      const html = (body.capture as { html?: string }).html ?? '';
      sizes.push(utf8Length(html));
      return sizes.length === 1 ? { ok: false, error: { code: 'too_large', message: 'capture exceeds 4 MiB', retryable: false } } : okOf(parsePreview(previewResult()));
    },
  });
  await ctl.start();
  await ctl.save();
  assert.equal(sizes.length, 2);
  assert.ok((sizes[1] ?? 0) <= Math.floor((sizes[0] ?? 0) * 0.75));
  assert.equal(ctl.state.phase.kind as string, 'review');
  assert.equal(ctl.state.snapshot?.html_truncated, true);
  assert.ok(!(shrinkSnapshot(SNAPSHOT).html ?? '').includes('�'));
});

test('corrections: clearing removes page tags, the title cannot be cleared, the container type is kept', () => {
  assert.throws(() => normalizeCorrections({ title: '  ' }), /title cannot be empty/);
  const cleared = normalizeCorrections({ doi: '', authors: [' '], year: '' });
  assert.deepEqual(clearedFields(cleared), ['doi', 'year', 'authors']);
  const r = buildCaptureRequest(SNAPSHOT, { collection: null, tags: [] }, true, cleared);
  assert.ok(!r.meta.some((m) => m.name === 'citation_doi'), 'page DOI removed, none added');
  const j = buildCaptureRequest(SNAPSHOT, { collection: null, tags: [] }, true, normalizeCorrections({ journal: 'Proc. Corrected' }));
  assert.deepEqual(j.meta[0], { name: 'citation_conference_title', content: 'Proc. Corrected' });
  assert.equal(j.meta.filter((m) => m.name === 'citation_conference_title').length, 1);
  const plain = buildCaptureRequest({ ...SNAPSHOT, meta: [] }, { collection: null, tags: [] }, true, { journal: 'J' });
  assert.equal(plain.meta[0]?.name, 'citation_journal_title');
});

test('the popup says when a corrected DOI was ignored or a cleared field came back', () => {
  let s = reduce(initialState(), { type: 'extracted', outcome: okOf(SNAPSHOT) });
  s = reduce(s, { type: 'previewed', outcome: okOf(parsePreview(previewResult())), destination: { collection: null, tags: [] }, corrections: { doi: 'not a doi', journal: '' } });
  const notes = correctionNotes(s).map((n) => n.id);
  assert.deepEqual(notes, ['doi', 'cleared']);
  s = reduce(s, { type: 'previewed', outcome: okOf(parsePreview(previewResult())), destination: { collection: null, tags: [] }, corrections: { doi: 'https://doi.org/10.9999/X' } });
  assert.deepEqual(correctionNotes(s), [], 'accepted (prefix and case ignored)');
});

test('dropped meta/JSON-LD and the PDF host are shown in the review', async () => {
  const doc = dom('<!doctype html><html><body><main id="app"></main></body></html>');
  const root = doc.getElementById('app') as HTMLElement;
  const { ctl } = controller(base());
  ctl.subscribe((s) => render(root, s, ctl));
  await ctl.start();
  await ctl.save();
  assert.deepEqual(snapshotHints(ctl.state).map((h) => h.id), ['meta', 'json_ld']);
  assert.ok(root.querySelector('[data-hint="meta"]'));
  assert.ok(root.querySelector('[data-hint="json_ld"]'));
  assert.equal(root.querySelector('#pdf-0 ~ .pdf-host, label[for="pdf-0"] .pdf-host')?.textContent, 'cdn.example.org');
  assert.equal((root.querySelector('#pdf-0') as HTMLInputElement).checked, true, 'first candidate preselected');
});

// ---- extension IDs -----------------------------------------------------------------------

const DEV: ExtensionIds['development'] = { chrome: ['baapgkfjfjjagdkpkmocjhfopmaohcdc'], firefox: ['citation-manager-dev@citation-manager.invalid'] };
const ids = (release: ExtensionIds['release']): ExtensionIds => ({ development: DEV, release });

test('release IDs are validated', () => {
  checkIds(ids({ chrome: [], firefox: [] }));
  checkIds(ids({ chrome: ['abcdefghijklmnopabcdefghijklmnop'], firefox: ['capture@example.org', '{12345678-1234-1234-1234-123456789abc}'] }));
  for (const bad of [
    { chrome: [''], firefox: [] },
    { chrome: ['ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP'], firefox: [] },
    { chrome: ['abcdefghijklmnopqrstuvwxyzabcdef'], firefox: [] },
    { chrome: [], firefox: [''] },
    { chrome: [], firefox: ['not an id'] },
    { chrome: [], firefox: ['{not-a-uuid}'] },
    { chrome: ['baapgkfjfjjagdkpkmocjhfopmaohcdc'], firefox: [] },
    { chrome: [], firefox: ['citation-manager-dev@citation-manager.invalid'] },
  ]) {
    assert.throws(() => checkIds(ids(bad)), JSON.stringify(bad));
    assert.throws(() => buildManifest('chrome', 'development', '0.1.0', ids(bad)));
  }
});

test('Firefox minimum version and CSP', () => {
  const ff = buildManifest('firefox', 'development', '0.1.0', ids({ chrome: [], firefox: [] }));
  assert.equal((ff.browser_specific_settings as { gecko: { strict_min_version: string } }).gecko.strict_min_version, '140.0');
  for (const target of ['chrome', 'firefox'] as const) {
    const csp = (buildManifest(target, 'development', '0.1.0', ids({ chrome: [], firefox: [] })).content_security_policy as { extension_pages: string }).extension_pages;
    assert.match(csp, /connect-src 'none'/);
    assert.match(csp, /form-action 'none'/);
    assert.match(csp, /script-src 'self'/);
  }
});
