// Popup state machine and controller flow with a fake background, plus DOM rendering.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { Controller, type Client } from '../../src/popup/controller';
import { DUPLICATE_MESSAGE, DUPLICATE_SKIPPED_MESSAGE, MAX_POLLS, canCommit, initialState, pdfView, recoveryFor, reduce } from '../../src/popup/state';
import { collectionTree, render } from '../../src/popup/view';
import { buildCaptureRequest, normalizeCorrections } from '../../src/shared/request';
import type { Outcome, PageSnapshot } from '../../src/shared/types';
import { parseCommit, parseDestinations, parseHello, parseJob, parsePreview } from '../../src/shared/validate';
import { dom } from './helpers';
import { CHILD, COLLECTION, DESTINATIONS, HELLO, JOB, REDACTED_WARNING, TAG, commitResult, job, previewResult, skippedResult } from './samples';

const SNAPSHOT: PageSnapshot = {
  page_url: 'https://journal.example.org/a',
  title: 'Doc title',
  html: '<html><head><title>Doc title</title></head></html>',
  meta: [
    { name: 'citation_title', content: 'Page title' },
    { name: 'citation_author', content: 'Page, Author' },
    { name: 'DC.title', content: 'DC title' },
  ],
  json_ld: [],
  pdf_url: null,
  content_type: 'text/html',
  html_truncated: false,
  dropped_meta: 0,
  dropped_json_ld: 0,
};

const kindOf = (ctl: Controller): string => ctl.state.phase.kind;
const okOf = <T>(result: T): Outcome<T> => ({ ok: true, result });
const errOf = (code: string, retryable = false): Outcome<never> => ({ ok: false, error: { code, message: code, retryable } });

type Script = Record<string, (body: Record<string, unknown>) => Outcome<unknown>>;

function harness(script: Script) {
  const calls: Record<string, unknown>[] = [];
  const client: Client = {
    request: async (body) => {
      calls.push(body as Record<string, unknown>);
      const handler = script[body.op];
      if (!handler) throw new Error(`unexpected ${body.op}`);
      return handler(body as Record<string, unknown>);
    },
  };
  const remembered: unknown[] = [];
  const ctl = new Controller({ client, tabId: async () => 7, sleep: async () => undefined, remember: (d, o) => remembered.push([d, o]) });
  return { ctl, calls, remembered, ops: () => calls.map((c) => c.op) };
}

const happy = (): Script => ({
  hello: () => okOf(parseHello(HELLO)),
  destinations: () => okOf(parseDestinations(DESTINATIONS)),
  extract: () => okOf(SNAPSHOT),
  preview: () => okOf(parsePreview(previewResult())),
  commit: () => okOf(parseCommit(commitResult())),
  pdf_job: () => okOf(parseJob(job('running'))),
  retry_pdf: () => okOf(parseJob(job('queued', 0))),
  cancel: () => okOf({ cancelled: true }),
});

test('start: hello then destinations; ready to save', async () => {
  const h = harness(happy());
  await h.ctl.start();
  assert.deepEqual(h.ops(), ['hello', 'destinations']);
  assert.equal(kindOf(h.ctl), 'ready');
  assert.equal(h.ctl.state.destinations?.collections.length, 2);
});

test('not paired / unauthorized show pairing instructions; missing host shows install help', async () => {
  const notPaired = harness({ ...happy(), hello: () => okOf(parseHello({ ...HELLO, paired: false })) });
  await notPaired.ctl.start();
  assert.equal(notPaired.ctl.state.phase.kind, 'pairing');
  assert.deepEqual(notPaired.ops(), ['hello'], 'no daemon call before pairing');
  const unauthorized = harness({ ...happy(), destinations: () => errOf('unauthorized') });
  await unauthorized.ctl.start();
  assert.equal(unauthorized.ctl.state.phase.kind, 'pairing');
  const missing = harness({ ...happy(), hello: () => errOf('host_missing') });
  await missing.ctl.start();
  assert.deepEqual(missing.ctl.state.phase, { kind: 'error', op: 'hello', error: { code: 'host_missing', message: 'host_missing', retryable: false }, recovery: 'install' });
  const future = harness({ ...happy(), hello: () => okOf(parseHello({ ...HELLO, protocol: 2 })) });
  await future.ctl.start();
  assert.equal(future.ctl.state.phase.kind === 'error' && future.ctl.state.phase.error.code, 'unsupported_version');
});

test('save → preview → review with defaults → commit → PDF polling stops after MAX_POLLS', async () => {
  const h = harness(happy());
  await h.ctl.start();
  await h.ctl.setCollection(COLLECTION);
  await h.ctl.setTag(TAG, true);
  await h.ctl.save();
  assert.equal(kindOf(h.ctl), 'review');
  assert.deepEqual(h.ctl.state.decisions, ['Create']);
  assert.equal(h.ctl.state.pdf, 0);
  const preview = h.calls.find((c) => c.op === 'preview') as { capture: { destination: unknown; offline: boolean; meta: unknown[] } };
  assert.deepEqual(preview.capture.destination, { collection: COLLECTION, tags: [TAG] });
  assert.equal(h.remembered.length, 1);
  await h.ctl.commit();
  const commit = h.calls.find((c) => c.op === 'commit');
  assert.deepEqual(commit, { op: 'commit', preview_id: 'preview-1', decisions: [{ candidate_index: 0, action: 'Create' }], pdf: 0 });
  assert.equal(kindOf(h.ctl), 'saved');
  assert.equal(h.ops().filter((o) => o === 'pdf_job').length, MAX_POLLS);
  const view = pdfView(h.ctl.state);
  assert.equal(view.kind === 'pending' && view.polling, false);
  // "Check again" polls once more round.
  await h.ctl.refreshJob();
  assert.equal(h.ops().filter((o) => o === 'pdf_job').length, 2 * MAX_POLLS);
});

test('PDF failure is reported apart from the save, and Retry re-queues the job', async () => {
  let n = 0;
  const h = harness({ ...happy(), pdf_job: () => okOf(parseJob(++n < 2 ? job('retry_wait', 2) : job('failed', 4, 'attempt limit 4 reached: network error'))) });
  await h.ctl.start();
  await h.ctl.save();
  await h.ctl.commit();
  assert.equal(kindOf(h.ctl), 'saved');
  assert.equal(h.ctl.state.saved?.paper !== null, true);
  const failed = pdfView(h.ctl.state);
  assert.equal(failed.kind, 'failed');
  n = -100; // the retried job keeps running
  await h.ctl.retryPdf();
  assert.ok(h.ops().includes('retry_pdf'));
  assert.equal(h.calls.find((c) => c.op === 'retry_pdf')?.job, JOB);
  assert.equal(pdfView(h.ctl.state).kind, 'pending');
});

test('PDF job not submitted: the paper is still saved', () => {
  const s = reduce(initialState(), { type: 'committed', outcome: okOf(parseCommit({ ...commitResult(null), pdf_error: 'capture PDF job refused' })) });
  assert.equal(s.phase.kind as string, 'saved');
  assert.deepEqual(pdfView(s), { kind: 'not_submitted', message: 'capture PDF job refused' });
});

test('daemon unavailable is retryable and Retry commits the same preview again', async () => {
  let first = true;
  const h = harness({
    ...happy(),
    commit: () => {
      if (first) {
        first = false;
        return errOf('daemon_unavailable', true);
      }
      return okOf(parseCommit(commitResult(null)));
    },
  });
  await h.ctl.start();
  await h.ctl.save();
  await h.ctl.commit();
  assert.equal(h.ctl.state.phase.kind === 'error' && h.ctl.state.phase.recovery, 'retry');
  await h.ctl.recover();
  assert.equal(kindOf(h.ctl), 'saved');
  const commits = h.calls.filter((c) => c.op === 'commit');
  assert.equal(commits.length, 2);
  assert.equal(commits[1]?.preview_id, 'preview-1');
});

test('conflict asks to preview again; a repeated conflict with hidden duplicates is a duplicate', async () => {
  let previews = 0;
  const h = harness({
    ...happy(),
    // Two rows, one a hidden duplicate the daemon cannot attribute to a row for us.
    preview: () => okOf(parsePreview(previewResult({ id: `p${++previews}`, rows: 2, hiddenDuplicates: 1, warnings: [REDACTED_WARNING] }))),
    commit: () => errOf('conflict', true),
  });
  await h.ctl.start();
  await h.ctl.save();
  await h.ctl.commit();
  assert.equal(h.ctl.state.phase.kind === 'error' && h.ctl.state.phase.recovery, 'repreview');
  await h.ctl.recover();
  assert.equal(kindOf(h.ctl), 'review');
  assert.equal(h.ctl.state.preview?.preview_id, 'p2');
  assert.equal(h.ops().filter((o) => o === 'extract').length, 1, 'the page is not read again');
  await h.ctl.commit();
  const phase = h.ctl.state.phase;
  assert.equal(phase.kind === 'error' && phase.error.code, 'duplicate');
  assert.equal(phase.kind === 'error' && phase.error.message, DUPLICATE_MESSAGE);
  assert.equal(phase.kind === 'error' && phase.recovery, 'back');
  h.ctl.dispatch({ type: 'back' });
  await h.ctl.cancel();
  assert.equal(kindOf(h.ctl), 'ready');
  assert.deepEqual(h.calls.at(-1), { op: 'cancel', preview_id: 'p2' });
});

test('a hidden duplicate defaults to Skip; saving it anyway is skipped by the daemon and reported', async () => {
  const h = harness({
    ...happy(),
    preview: () => okOf(parsePreview(previewResult({ hiddenDuplicates: 1, warnings: [REDACTED_WARNING] }))),
    commit: () => okOf(parseCommit(skippedResult())),
  });
  await h.ctl.start();
  await h.ctl.save();
  assert.equal(kindOf(h.ctl), 'review');
  assert.deepEqual(h.ctl.state.decisions, ['Skip']);
  assert.equal(canCommit(h.ctl.state), false, 'nothing to save as long as the duplicate is skipped');
  h.ctl.dispatch({ type: 'decision', index: 0, action: 'Create' });
  await h.ctl.commit();
  const phase = h.ctl.state.phase;
  assert.equal(phase.kind === 'error' && phase.error.code, 'duplicate');
  assert.equal(phase.kind === 'error' && phase.error.message, DUPLICATE_SKIPPED_MESSAGE);
  assert.equal(phase.kind === 'error' && phase.recovery, 'none');
  assert.equal(h.ctl.state.saved, null, 'nothing was saved');
  assert.equal(h.ctl.state.preview, null, 'the daemon used up the preview');
  assert.deepEqual(h.ops().filter((o) => o === 'commit' || o === 'pdf_job'), ['commit'], 'committed once, no PDF job polled');
  h.ctl.dispatch({ type: 'back' });
  assert.equal(kindOf(h.ctl), 'ready');
});

test('a skipped duplicate after an earlier save of another page does not show that save', () => {
  let s = reduce(initialState(), { type: 'extracted', outcome: okOf(SNAPSHOT) });
  s = reduce(s, { type: 'previewed', outcome: okOf(parsePreview(previewResult())), destination: { collection: null, tags: [] }, corrections: {} });
  s = reduce(s, { type: 'committed', outcome: okOf(parseCommit(commitResult())) });
  assert.equal(s.phase.kind, 'saved');
  s = reduce(s, { type: 'extracted', outcome: okOf(SNAPSHOT) });
  s = reduce(s, { type: 'previewed', outcome: okOf(parsePreview(previewResult({ hiddenDuplicates: 1 }))), destination: { collection: null, tags: [] }, corrections: {} });
  s = reduce(s, { type: 'decision', index: 0, action: 'Create' });
  s = reduce(s, { type: 'committed', outcome: okOf(parseCommit(skippedResult())) });
  assert.equal(s.phase.kind === 'error' && s.phase.error.code, 'duplicate');
  s = reduce(s, { type: 'back' });
  assert.equal(s.phase.kind, 'ready');
  // Skipping on purpose is not an error.
  s = reduce(s, { type: 'extracted', outcome: okOf(SNAPSHOT) });
  s = reduce(s, { type: 'previewed', outcome: okOf(parsePreview(previewResult({ rows: 2 }))), destination: { collection: null, tags: [] }, corrections: {} });
  s = reduce(s, { type: 'decision', index: 1, action: 'Skip' });
  s = reduce(s, { type: 'committed', outcome: okOf(parseCommit(commitResult())) });
  assert.equal(s.phase.kind, 'saved');
});

test('a visible exact duplicate defaults to Skip and cannot be committed as is', () => {
  const existing = { id: 'x', title: 'Old', external_ids: [], doi: '10.9999/x' };
  let s = reduce(initialState(), { type: 'extracted', outcome: okOf(SNAPSHOT) });
  s = reduce(s, { type: 'previewed', outcome: okOf(parsePreview(previewResult({ matches: [{ existing, kind: 'Doi', differences: [] }] }))), destination: { collection: null, tags: [] }, corrections: {} });
  assert.deepEqual(s.decisions, ['Skip']);
  assert.equal(s.phase.kind as string, 'review');
  s = reduce(s, { type: 'decision', index: 0, action: 'Create' });
  s = reduce(s, { type: 'committed', outcome: errOf('conflict', true) });
  assert.equal(s.phase.kind === 'error' && s.phase.error.code, 'duplicate');
});

test('changing the destination after a preview previews again', async () => {
  const h = harness(happy());
  await h.ctl.start();
  await h.ctl.save();
  await h.ctl.setCollection(CHILD);
  const previews = h.calls.filter((c) => c.op === 'preview') as { capture: { destination: { collection: string } } }[];
  assert.equal(previews.length, 2);
  assert.equal(previews[1]?.capture.destination.collection, CHILD);
  assert.equal(kindOf(h.ctl), 'review');
});

test('manual corrections replace the page tags and are sent as a new preview', async () => {
  const h = harness({ ...happy(), preview: () => okOf(parsePreview(previewResult({ provisional: true }))) });
  await h.ctl.start();
  await h.ctl.save();
  assert.equal(h.ctl.state.preview?.provisional, true);
  await h.ctl.correct({ title: '  Fixed title ', authors: ['Franklin, Rosalind', ' ', 'Meitner, Lise'], year: '2025' });
  const last = h.calls.filter((c) => c.op === 'preview').at(-1) as { capture: { meta: { name: string; content: string }[] } };
  assert.deepEqual(last.capture.meta, [
    { name: 'citation_title', content: 'Fixed title' },
    { name: 'citation_author', content: 'Franklin, Rosalind' },
    { name: 'citation_author', content: 'Meitner, Lise' },
    { name: 'citation_publication_date', content: '2025' },
    { name: 'DC.title', content: 'DC title' },
  ]);
  assert.deepEqual(h.ctl.state.corrections.year, '2025');
  await h.ctl.correct({ year: '20x5' });
  assert.equal(h.ctl.state.phase.kind === 'error' && h.ctl.state.phase.error.code, 'invalid_input');
  assert.equal(h.ctl.state.phase.kind === 'error' && h.ctl.state.phase.recovery, 'back');
});

test('correction helpers', () => {
  assert.deepEqual(normalizeCorrections({ doi: ' 10.9999/y ' }), { doi: '10.9999/y' });
  assert.throws(() => normalizeCorrections({ year: '99' }));
  const r = buildCaptureRequest(SNAPSHOT, { collection: null, tags: [] }, true, { doi: '10.9999/z' });
  assert.equal(r.meta[0]?.name, 'citation_doi');
  assert.equal(r.offline, true);
  assert.equal(r.html, SNAPSHOT.html);
});

test('extraction failures and recovery mapping', async () => {
  const h = harness({ ...happy(), extract: () => errOf('no_access') });
  await h.ctl.start();
  await h.ctl.save();
  assert.equal(h.ctl.state.phase.kind === 'error' && h.ctl.state.phase.recovery, 'none');
  assert.ok(!h.ops().includes('preview'));
  assert.equal(recoveryFor({ code: 'busy', message: '', retryable: true }, 'preview', false), 'retry');
  assert.equal(recoveryFor({ code: 'not_found', message: '', retryable: false }, 'commit', true), 'repreview');
  assert.equal(recoveryFor({ code: 'bad_reply', message: '', retryable: false }, 'preview', false), 'none');
});

test('remembered destinations that no longer exist are dropped', () => {
  const s = reduce(initialState({ collection: '01a20f9f-0000-7000-8000-00000000dead', tags: [TAG, '01a20f9f-0000-7000-8000-00000000beef'] }), {
    type: 'destinations',
    outcome: okOf(parseDestinations(DESTINATIONS)),
  });
  assert.deepEqual(s.destination, { collection: null, tags: [TAG] });
});

test('collections render as a tree', () => {
  assert.deepEqual(collectionTree(parseDestinations(DESTINATIONS).collections).map(({ c, depth }) => [c.name, depth]), [
    ['Reading', 0],
    ['Child', 1],
  ]);
});

const change = (node: Element) => new ((node.ownerDocument.defaultView as unknown as { Event: typeof Event }).Event)('change');

// ---- rendering -------------------------------------------------------------------------

function view(stateOf: (ctl: Controller) => Promise<void>, script: Script = happy()) {
  const doc = dom('<!doctype html><html><body><main id="app"></main></body></html>');
  const root = doc.getElementById('app') as HTMLElement;
  const h = harness(script);
  h.ctl.subscribe((s) => render(root, s, h.ctl));
  return { doc, root, h, ready: stateOf(h.ctl) };
}

test('review UI shows fields, PDF choices, destination, decisions; page strings stay text', async () => {
  const evil = '<img src=x onerror=alert(1)>';
  const { doc, root, ready } = view(
    async (ctl) => {
      await ctl.start();
      await ctl.save();
    },
    { ...happy(), preview: () => okOf(parsePreview(previewResult({ title: evil, rows: 3, hiddenDuplicates: 2, warnings: [REDACTED_WARNING, 'Other note'] }))) },
  );
  await ready;
  assert.equal(root.dataset.phase, 'review');
  assert.equal(root.querySelector('[data-field="title"]')?.textContent, evil);
  assert.equal(root.querySelectorAll('img').length, 0, 'no markup from page/host strings');
  assert.equal(root.querySelector('[data-field="doi"]')?.textContent, '10.9999/x');
  assert.equal(root.querySelector('[data-field="authors"]')?.textContent, 'Ada Lovelace');
  assert.equal(root.querySelectorAll('input[name="pdf"]').length, 2);
  assert.equal((root.querySelector('#pdf-0') as HTMLInputElement).checked, true);
  assert.equal((root.querySelector('#decision-0-Create') as HTMLInputElement).checked, true);
  assert.equal(root.querySelector('#hidden-matches')?.getAttribute('data-count'), '2');
  assert.match(root.querySelector('#hidden-matches')?.textContent ?? '', /Already in your library: 2 /);
  // The constant redaction note is summarized by the line above; other notes are shown.
  const notes = [...root.querySelectorAll('p.hint.warn')].map((p) => p.textContent);
  assert.ok(!notes.some((n) => n === REDACTED_WARNING), JSON.stringify(notes));
  assert.ok(notes.includes('Other note'), JSON.stringify(notes));
  assert.ok(root.querySelector('#collection'));
  assert.ok(root.querySelector('#fix-title'));
  assert.equal((root.querySelector('#commit') as HTMLButtonElement).hasAttribute('disabled'), false);
  void doc;
});

test('clicking Skip disables commit; choosing no PDF sends pdf null', async () => {
  const { root, h, ready } = view(async (ctl) => {
    await ctl.start();
    await ctl.save();
  });
  await ready;
  (root.querySelector('#decision-0-Skip') as HTMLInputElement).dispatchEvent(change(root));
  assert.deepEqual(h.ctl.state.decisions, ['Skip']);
  assert.equal(root.querySelector('#commit')?.hasAttribute('disabled'), true);
  (root.querySelector('#decision-0-Create') as HTMLInputElement).dispatchEvent(change(root));
  (root.querySelector('#pdf-none') as HTMLInputElement).dispatchEvent(change(root));
  (root.querySelector('#commit') as HTMLButtonElement).click();
  await new Promise((r) => setTimeout(r, 10));
  assert.equal(h.calls.find((c) => c.op === 'commit')?.pdf, null);
  assert.equal(root.dataset.phase, 'saved');
  assert.match(root.querySelector('#saved')?.textContent ?? '', /Saved to your library/);
});

test('pairing, error and PDF failure views offer the right actions', async () => {
  const pairing = view((ctl) => ctl.start(), { ...happy(), hello: () => okOf(parseHello({ ...HELLO, paired: false })) });
  await pairing.ready;
  assert.equal(pairing.root.dataset.phase, 'pairing');
  assert.match(pairing.root.textContent ?? '', /papers-native-host pair/);
  assert.ok(pairing.root.querySelector('#check-again'));

  const busy = view((ctl) => ctl.start(), { ...happy(), destinations: () => errOf('busy', true) });
  await busy.ready;
  assert.equal(busy.root.querySelector('#error')?.getAttribute('data-code'), 'busy');
  assert.equal(busy.root.querySelector('#recover')?.textContent, 'Retry');

  const failed = view(
    async (ctl) => {
      await ctl.start();
      await ctl.save();
      await ctl.commit();
    },
    { ...happy(), pdf_job: () => okOf(parseJob(job('failed', 4, 'auth required'))) },
  );
  await failed.ready;
  assert.equal(failed.root.querySelector('#pdf-status')?.getAttribute('data-state'), 'failed');
  assert.match(failed.root.querySelector('#pdf-status')?.textContent ?? '', /auth required/);
  assert.ok(failed.root.querySelector('#retry-pdf'));
  assert.ok(failed.root.querySelector('#saved'), 'the save itself is reported as done');
});
