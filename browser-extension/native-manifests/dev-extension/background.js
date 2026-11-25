// Development-only smoke extension for papers-native-host (#23); NOT the #24 extension.
// On startup it reads smoke-config.json (written by scripts/native-host/browser_smoke.py),
// runs the native messaging contract once with both one-shot messages and a port, and
// POSTs what it saw to the local smoke driver. Runs unchanged in Chrome (MV3 service
// worker) and Firefox (MV2 background script).
const HOST = 'dev.citation_manager.native_host';
const api = globalThis.browser ?? globalThis.chrome;

function sendOnce(message) {
  // Firefox: promise API. Chrome: callback form (also valid in MV3).
  if (globalThis.browser) return globalThis.browser.runtime.sendNativeMessage(HOST, message);
  return new Promise((resolve, reject) => {
    chrome.runtime.sendNativeMessage(HOST, message, (reply) => {
      const error = chrome.runtime.lastError;
      if (error) reject(new Error(error.message));
      else resolve(reply);
    });
  });
}

// Sends messages one at a time over a long-lived port, then disconnects it.
function viaPort(messages) {
  return new Promise((resolve, reject) => {
    const port = api.runtime.connectNative(HOST);
    const replies = [];
    let finished = false;
    port.onMessage.addListener((reply) => {
      replies.push(reply);
      if (replies.length === messages.length) {
        finished = true;
        port.disconnect();
        resolve(replies);
      } else {
        port.postMessage(messages[replies.length]);
      }
    });
    port.onDisconnect.addListener((p) => {
      if (!finished) {
        const error = p.error ?? globalThis.chrome?.runtime?.lastError;
        reject(new Error(`port closed early: ${error?.message ?? 'unknown'}`));
      }
    });
    port.postMessage(messages[0]);
  });
}

async function run() {
  const config = await (await fetch(api.runtime.getURL('smoke-config.json'))).json();
  const base = `http://127.0.0.1:${config.port}`;
  const steps = {};
  try {
    const html = await (await fetch(`${base}/fixture.html`)).text();
    steps.hello = await sendOnce({ v: 1, id: 'hello', type: 'hello' });
    steps.preview = await sendOnce({
      v: 1,
      id: 'preview',
      type: 'capture.preview',
      capture: { page_url: config.page_url, title: config.title, html, offline: true },
    });
    if (!steps.preview.ok) throw new Error(`preview failed: ${JSON.stringify(steps.preview.error)}`);
    const preview = steps.preview.result;
    const pdf = preview.resolution.pdf_candidates.length > 0 ? 0 : null;
    // The capture grant has no LibraryWrite: the daemon must refuse a merge.
    steps.merge_denied = await sendOnce({
      v: 1,
      id: 'merge',
      type: 'capture.commit',
      preview_id: preview.preview.id,
      decisions: [{ candidate_index: 0, action: { Merge: { match_index: 0, fields: ['title'] } } }],
    });
    const [destinations, commit] = await viaPort([
      { v: 1, id: 'destinations', type: 'destinations' },
      {
        v: 1,
        id: 'commit',
        type: 'capture.commit',
        preview_id: preview.preview.id,
        decisions: [{ candidate_index: 0, action: 'Create' }],
        pdf,
      },
    ]);
    steps.destinations = destinations;
    steps.commit = commit;
    steps.unsupported_version = await sendOnce({ v: 99, id: 'v99', type: 'hello' });
    steps.not_a_proxy = await sendOnce({ v: 1, id: 'proxy', type: 'grants.issue' });
    await fetch(`${base}/report`, { method: 'POST', body: JSON.stringify({ ok: true, steps }) });
  } catch (error) {
    await fetch(`${base}/report`, {
      method: 'POST',
      body: JSON.stringify({ ok: false, error: String(error), steps }),
    });
  }
}

run();
