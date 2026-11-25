#!/usr/bin/env node
// GUI-less end-to-end test of the #24 extension (Linux, X display such as Xvfb :99):
//
//   real browser + built extension -> papers-native-host -> papersd (started on demand)
//
// Everything runs in a throw-away HOME/config/library/runtime directory with the binaries
// copied into a temp prefix; the user's library and browser profiles are never touched.
//  1. `papers-native-host register --pair` (development IDs) into the temp browser profile;
//     wait until the papersd started by pairing has idled out (no daemon, no GUI running).
//  2. Serve the HTML fixtures on http://127.0.0.1:<port>; open the article in the browser.
//  3. Press the extension's keyboard shortcut with xdotool: a real user gesture, so the
//     browser grants activeTab for that tab and opens the popup.
//  4. Drive the popup: Save (offline DOI lookup off) -> review -> commit -> PDF job status ->
//     Retry PDF; then a duplicate save of the same page (skipped by the daemon, nothing
//     created); then a page without metadata saved provisionally with manual corrections.
//  5. Quit the browser, check no host process is left, and open the library as its owner
//     (as the app does on launch) to confirm both papers were persisted.
//
// Chromium is driven with Playwright; the toolbar popup is not exposed as a Playwright page,
// so after the shortcut granted activeTab the popup document is opened in a tab with
// `?tab=<id>` (the grant belongs to the extension, not to the popup window). Firefox ESR is
// driven over WebDriver BiDi (Node's built-in WebSocket, `--remote-allow-system-access` so
// a moz-extension page can be opened); X key presses do not reach Firefox under Xvfb without
// a window manager, so the same shortcut is delivered as a trusted key event inside the
// browser window (nsITextInputProcessor, as Mozilla's own tests do). Before the first
// gesture each run checks the negative case: the tab URL is hidden and Save gets no_access.
//
//   node tests/e2e/run.mjs --browser chromium --bin-dir DIR [--executable PATH]
//   node tests/e2e/run.mjs --browser firefox --bin-dir DIR --executable /path/to/firefox
// DIR holds papersd, papers-native-host and examples/inspect_capture
// (`cargo build -p cm-daemon --bin papersd -p cm-native-host --bins --examples`).
import { spawn, spawnSync, execFileSync } from 'node:child_process';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import {
  chmodSync,
  copyFileSync,
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readdirSync,
  readlinkSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { deflateRawSync } from 'node:zlib';

const here = dirname(fileURLToPath(import.meta.url));
const extRoot = resolve(here, '../..');
const ids = JSON.parse(readFileSync(join(extRoot, 'native-manifests/extension-ids.json'), 'utf8'));
const CHROME_ID = ids.development.chrome[0];
const GECKO_ID = ids.development.firefox[0];
const FIREFOX_UUID = '6f1f5a2e-24e2-4c1a-9a43-0000000000e2';
const CORRECTED_TITLE = 'Lab Notes on Provisional Capture (corrected)';

function parseArgs() {
  const argv = process.argv.slice(2);
  const get = (flag, fallback) => {
    const i = argv.indexOf(flag);
    return i >= 0 ? argv[i + 1] : fallback;
  };
  const browser = get('--browser');
  if (browser !== 'chromium' && browser !== 'firefox') throw new Error('--browser chromium|firefox');
  const binDir = get('--bin-dir');
  if (!binDir) throw new Error('--bin-dir is required');
  return {
    browser,
    binDir: resolve(binDir),
    executable: get('--executable', browser === 'chromium' ? '/opt/pw-browsers/chromium-1243/chrome-linux64/chrome' : undefined),
    keep: argv.includes('--keep'),
    report: get('--report'),
  };
}

const log = (message) => console.log(`[e2e] ${message}`);
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

function processesRunning(executable) {
  const found = [];
  for (const entry of readdirSync('/proc')) {
    if (!/^\d+$/.test(entry)) continue;
    try {
      if (readlinkSync(`/proc/${entry}/exe`) === executable) found.push(Number(entry));
    } catch {
      // gone or not ours
    }
  }
  return found;
}

async function waitFor(condition, timeoutMs, stepMs = 250) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await condition()) return true;
    await sleep(stepMs);
  }
  return condition();
}

// ---- fixture server --------------------------------------------------------------------

function serveFixtures() {
  const pages = {
    '/article.html': readFileSync(join(here, 'fixtures/article.html')),
    '/no-metadata.html': readFileSync(join(here, 'fixtures/no-metadata.html')),
  };
  const server = createServer((req, res) => {
    const body = pages[req.url ?? ''];
    if (!body) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8', 'Content-Length': body.length }).end(body);
  });
  return new Promise((ok) => server.listen(0, '127.0.0.1', () => ok(server)));
}

// ---- xdotool: the user's keyboard shortcut ---------------------------------------------

function pressShortcut(windowPattern) {
  const out = spawnSync('xdotool', ['search', '--onlyvisible', '--name', windowPattern], { encoding: 'utf8' });
  const windows = out.stdout.split('\n').filter(Boolean);
  if (windows.length === 0) throw new Error(`no visible window matching ${windowPattern}`);
  const win = windows[windows.length - 1];
  spawnSync('xdotool', ['windowactivate', '--sync', win], { stdio: 'ignore' });
  spawnSync('xdotool', ['windowfocus', '--sync', win], { stdio: 'ignore' });
  execFileSync('xdotool', ['key', '--clearmodifiers', 'alt+shift+s']);
}

// ---- popup scenario (browser-neutral: only `evaluate(expression)` is needed) ------------

const q = (selector) => JSON.stringify(selector);
const click = (selector) => `(() => { const e = document.querySelector(${q(selector)}); if (!e) return false; e.click(); return true; })()`;

async function until(popup, expression, timeoutMs, what) {
  let last;
  const ok = await waitFor(async () => {
    try {
      last = await popup.evaluate(expression);
      return Boolean(last);
    } catch (error) {
      last = String(error);
      return false;
    }
  }, timeoutMs, 300);
  if (!ok) {
    const phase = await popup.evaluate(`document.getElementById('app')?.dataset.phase + ' :: ' + document.getElementById('app')?.innerText`).catch(String);
    throw new Error(`timed out waiting for ${what} (last ${JSON.stringify(last)}; popup ${phase})`);
  }
  return last;
}

const phaseIs = (kind) => `document.getElementById('app')?.dataset.phase === ${q(kind)}`;
const text = (selector) => `document.querySelector(${q(selector)})?.textContent ?? null`;

async function saveToReview(popup, checks, prefix) {
  await until(popup, `${phaseIs('ready')} || ${phaseIs('pairing')} || ${phaseIs('error')}`, 30000, 'ready');
  checks[`${prefix}_popup_ready_paired`] = await popup.evaluate(phaseIs('ready'));
  if (!checks[`${prefix}_popup_ready_paired`]) throw new Error(`popup not ready: ${await popup.evaluate(text('#app'))}`);
  // Offline: no Crossref/DataCite lookup during preview (the sandbox has no internet).
  await popup.evaluate(`(() => { const o = document.getElementById('offline'); if (o && !o.checked) o.click(); return true; })()`);
  await popup.evaluate(click('#save'));
  await until(popup, `${phaseIs('review')} || ${phaseIs('error')}`, 60000, 'review');
  if (await popup.evaluate(phaseIs('error'))) throw new Error(`preview failed: ${await popup.evaluate(text('#error'))}`);
}

const reviewFields = `(() => {
  const f = {};
  for (const dd of document.querySelectorAll('[data-candidate="0"] dd')) f[dd.dataset.field] = dd.textContent;
  f.pdfs = [...document.querySelectorAll('input[name="pdf"]')].map((i) => ({ value: i.value, checked: i.checked, label: i.parentElement.textContent }));
  f.decision = document.querySelector('input[name="decision-0"]:checked')?.value ?? null;
  f.provisional = Boolean(document.getElementById('provisional'));
  f.warnings = [...document.querySelectorAll('.hint.warn')].map((p) => p.textContent);
  f.matches = [...document.querySelectorAll('.matches li')].map((li) => li.textContent);
  f.commitEnabled = !document.getElementById('commit')?.disabled;
  return f;
})()`;

async function articleScenario(popup, checks, results) {
  await saveToReview(popup, checks, 'article');
  const fields = await popup.evaluate(reviewFields);
  results.article_review = fields;
  checks.article_preview_title = fields.title === 'Capturing Papers With the Desktop App Closed';
  checks.article_preview_doi = fields.doi === '10.9999/e2e24.capture';
  checks.article_preview_year = fields.year === '2026';
  checks.article_preview_authors = /Ada Lovelace/.test(fields.authors) && /Grace Hopper/.test(fields.authors);
  checks.article_pdf_candidate_listed = fields.pdfs.some((p) => p.value === '0' && p.checked && /e2e24-fixture/.test(p.label));
  checks.article_default_decision_create = fields.decision === 'Create';
  await popup.evaluate(click('#commit'));
  await until(popup, `${phaseIs('saved')} || ${phaseIs('error')}`, 60000, 'saved');
  if (await popup.evaluate(phaseIs('error'))) throw new Error(`commit failed: ${await popup.evaluate(text('#error'))}`);
  const paper = await popup.evaluate(`document.getElementById('saved')?.dataset.paper ?? null`);
  checks.article_saved_with_paper_id = /^[0-9a-f-]{36}$/.test(paper ?? '');
  results.article_paper = paper;
  // The PDF host does not resolve offline: the job fails after its bounded retries, while
  // the paper stays saved. Keep checking until it is terminal, then retry explicitly.
  const states = [];
  const terminal = await waitFor(async () => {
    const state = await popup.evaluate(`document.getElementById('pdf-status')?.dataset.state ?? null`);
    if (states.at(-1) !== state) states.push(state);
    if (state === 'failed' || state === 'attached' || state === 'not_submitted') return true;
    await popup.evaluate(click('#refresh-pdf'));
    return false;
  }, 240000, 3000);
  results.article_pdf_states = states;
  results.article_pdf_text = await popup.evaluate(text('#pdf-status'));
  checks.article_pdf_failure_reported_separately = terminal && states.at(-1) === 'failed' && (await popup.evaluate(text('#saved'))) !== null;
  checks.article_retry_button_shown = await popup.evaluate(`Boolean(document.getElementById('retry-pdf'))`);
  if (checks.article_retry_button_shown) {
    await popup.evaluate(click('#retry-pdf'));
    await until(popup, `${phaseIs('saved')} && !document.getElementById('working')`, 30000, 'retry result');
    await sleep(500);
    results.article_after_retry = await popup.evaluate(text('#pdf-status'));
    // An explicit retry resets the attempt counter and re-queues the job.
    checks.article_retry_requeued = /PDF download (queued|running|retry wait) \(attempt [01] of|PDF download failed|download failed/.test(results.article_after_retry ?? '');
  }
}

async function duplicateScenario(popup, checks, results) {
  await saveToReview(popup, checks, 'duplicate');
  const fields = await popup.evaluate(reviewFields);
  results.duplicate_review = fields;
  // The capture grant has no LibraryRead: the match is hidden, but the daemon reports the
  // row as a hidden exact duplicate (`hidden_duplicates`) and the popup says so.
  const hidden = await popup.evaluate(`document.getElementById('hidden-matches')?.dataset.count ?? null`);
  results.duplicate_hidden_count = hidden;
  checks.duplicate_preview_flags_existing_match = fields.matches.length > 0 || hidden === '1';
  // It defaults to Skip, so there is nothing to save as is.
  checks.duplicate_defaults_to_skip = fields.decision === 'Skip' && fields.commitEnabled === false;
  // Save as new anyway: the daemon skips the hidden exact duplicate (no conflict, nothing
  // created, no PDF job) and the popup reports the existing paper.
  await popup.evaluate(click('#decision-0-Create'));
  await until(popup, `document.querySelector('input[name="decision-0"]:checked')?.value === 'Create' && !document.getElementById('commit')?.disabled`, 10000, 'save as new chosen');
  await popup.evaluate(click('#commit'));
  await until(popup, `${phaseIs('saved')} || ${phaseIs('error')}`, 60000, 'duplicate commit result');
  const outcome = await popup.evaluate(`({ phase: document.getElementById('app').dataset.phase, code: document.getElementById('error')?.dataset.code ?? null, message: document.getElementById('error')?.textContent ?? null, paper: document.getElementById('saved')?.dataset.paper ?? null, recover: document.getElementById('recover')?.textContent ?? null })`);
  results.duplicate_commit = outcome;
  checks.duplicate_skipped_not_conflict = outcome.phase === 'error' && outcome.code === 'duplicate' && /nothing was saved/.test(outcome.message ?? '');
  checks.duplicate_reported_not_created = outcome.phase === 'error' && outcome.code === 'duplicate' && outcome.paper === null && outcome.recover === null;
  if (outcome.phase === 'error') {
    await popup.evaluate(click('#back'));
    await until(popup, phaseIs('ready'), 15000, 'back to ready');
    checks.duplicate_back_to_ready = true;
  }
}

async function provisionalScenario(popup, checks, results) {
  await saveToReview(popup, checks, 'provisional');
  const fields = await popup.evaluate(reviewFields);
  results.provisional_review = fields;
  checks.provisional_flagged = fields.provisional === true;
  const set = (id, value) => `(() => { const e = document.getElementById(${q(id)}); e.value = ${JSON.stringify(value)}; return true; })()`;
  await popup.evaluate(set('fix-title', CORRECTED_TITLE));
  await popup.evaluate(set('fix-authors', 'Franklin, Rosalind\nMeitner, Lise'));
  await popup.evaluate(set('fix-year', '2025'));
  await popup.evaluate(click('#apply-corrections'));
  await until(popup, `${phaseIs('review')} && document.querySelector('[data-field="title"]')?.textContent === ${q(CORRECTED_TITLE)}`, 60000, 'corrected preview');
  const corrected = await popup.evaluate(reviewFields);
  results.provisional_corrected = corrected;
  checks.correction_applied_by_daemon = corrected.year === '2025' && /Rosalind Franklin/.test(corrected.authors) && corrected.provisional === false;
  await popup.evaluate(click('#commit'));
  await until(popup, `${phaseIs('saved')} || ${phaseIs('error')}`, 60000, 'provisional saved');
  results.provisional_paper = await popup.evaluate(`document.getElementById('saved')?.dataset.paper ?? null`);
  checks.provisional_saved = /^[0-9a-f-]{36}$/.test(results.provisional_paper ?? '');
}

async function negativeControl(probe, tabs, pageUrl) {
  await until(probe, `${phaseIs('ready')} || ${phaseIs('pairing')} || ${phaseIs('error')}`, 30000, 'probe ready');
  await probe.evaluate(click('#save'));
  await until(probe, `${phaseIs('error')} || ${phaseIs('review')}`, 30000, 'probe save result');
  return {
    tab_urls_visible: tabs.filter((t) => t.url === pageUrl).length,
    save_error: await probe.evaluate(`document.getElementById('error')?.dataset.code ?? document.getElementById('app').dataset.phase`),
  };
}

// ---- Chromium (Playwright) -------------------------------------------------------------

function loadPlaywright() {
  const attempts = [join(extRoot, 'package.json'), '/opt/node-tools/package.json', join(process.cwd(), 'package.json')];
  for (const from of attempts) {
    try {
      return createRequire(from)('playwright');
    } catch {
      // next
    }
  }
  throw new Error('playwright is not installed (it is not a dependency of this package)');
}

async function runChromium(env, ctx) {
  const { chromium } = loadPlaywright();
  const profile = ctx.chromiumProfile;
  for (const [k, v] of Object.entries(env)) process.env[k] = v;
  const context = await chromium.launchPersistentContext(profile, {
    executablePath: ctx.executable,
    headless: false,
    env,
    args: [`--disable-extensions-except=${ctx.extDir}`, `--load-extension=${ctx.extDir}`, '--no-first-run', '--no-default-browser-check', '--disable-gpu'],
    ignoreDefaultArgs: ['--disable-extensions', '--disable-component-extensions-with-background-pages'],
    viewport: null,
  });
  const browser = {
    close: () => context.close(),
    pid: null,
  };
  const page = context.pages()[0] ?? (await context.newPage());

  const openPopup = async (pageUrl, title, { control = false } = {}) => {
    await page.goto(pageUrl);
    if (control) {
      // Negative control before any user gesture: no activeTab grant, so the tab URL is
      // hidden from the extension and Save cannot read the page.
      const probe = await context.newPage();
      await probe.goto(`chrome-extension://${CHROME_ID}/popup.html`);
      const tabs = await probe.evaluate(async () => (await chrome.tabs.query({})).map((t) => ({ id: t.id, url: t.url ?? null })));
      const selfId = await probe.evaluate(async () => (await chrome.tabs.getCurrent())?.id ?? null);
      const target = tabs.find((t) => t.id !== selfId);
      await probe.goto(`chrome-extension://${CHROME_ID}/popup.html?tab=${target.id}`);
      ctx.results.control = await negativeControl({ evaluate: (e) => probe.evaluate(e) }, tabs, pageUrl);
      await probe.close();
    }
    await page.bringToFront();
    const before = new Set(context.pages());
    pressShortcut(title);
    let popup = null;
    await waitFor(() => {
      popup = context.pages().find((p) => !before.has(p) && p.url().includes('/popup.html')) ?? null;
      return popup !== null;
    }, 8000, 200);
    ctx.results.chromium_pages = context.pages().map((p) => p.url());
    let mode = 'toolbar-popup';
    if (!popup) {
      // Fallback: the shortcut granted activeTab; open the popup document in a tab.
      mode = 'popup-in-tab';
      popup = await context.newPage();
      await popup.goto(`chrome-extension://${CHROME_ID}/popup.html`);
      const tabId = await popup.evaluate(async (url) => (await chrome.tabs.query({})).find((t) => t.url === url)?.id ?? null, pageUrl);
      if (tabId === null) throw new Error('fixture tab not visible: activeTab was not granted');
      await popup.goto(`chrome-extension://${CHROME_ID}/popup.html?tab=${tabId}`);
    }
    return { mode, evaluate: (expression) => popup.evaluate(expression), close: () => popup.close().catch(() => undefined) };
  };
  return { browser, openPopup, context };
}

// ---- Firefox (WebDriver BiDi) ----------------------------------------------------------

function buildXpi(dir, out) {
  // Minimal ZIP (deflate) writer: no extra dependency.
  const files = [];
  const walk = (d, prefix = '') => {
    for (const e of readdirSync(d, { withFileTypes: true })) {
      if (e.isDirectory()) walk(join(d, e.name), `${prefix}${e.name}/`);
      else files.push([`${prefix}${e.name}`, readFileSync(join(d, e.name))]);
    }
  };
  walk(dir);
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    return c >>> 0;
  });
  const crc32 = (buf) => {
    let c = 0xffffffff;
    for (const b of buf) c = crcTable[(c ^ b) & 0xff] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const locals = [];
  const centrals = [];
  let offset = 0;
  for (const [name, data] of files) {
    const nameBuf = Buffer.from(name);
    const deflated = deflateRawSync(data);
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0);
    local.writeUInt16LE(20, 4);
    local.writeUInt16LE(8, 8);
    local.writeUInt32LE(crc, 14);
    local.writeUInt32LE(deflated.length, 18);
    local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBuf.length, 26);
    locals.push(local, nameBuf, deflated);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0);
    central.writeUInt16LE(20, 4);
    central.writeUInt16LE(20, 6);
    central.writeUInt16LE(8, 10);
    central.writeUInt32LE(crc, 16);
    central.writeUInt32LE(deflated.length, 20);
    central.writeUInt32LE(data.length, 24);
    central.writeUInt16LE(nameBuf.length, 28);
    central.writeUInt32LE(offset, 42);
    centrals.push(central, nameBuf);
    offset += 30 + nameBuf.length + deflated.length;
  }
  const centralSize = centrals.reduce((n, b) => n + b.length, 0);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0);
  end.writeUInt16LE(files.length, 8);
  end.writeUInt16LE(files.length, 10);
  end.writeUInt32LE(centralSize, 12);
  end.writeUInt32LE(offset, 16);
  writeFileSync(out, Buffer.concat([...locals, ...centrals, end]));
}

class Bidi {
  constructor(url) {
    this.next = 1;
    this.pending = new Map();
    this.ws = new WebSocket(url);
    this.ws.onmessage = (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { ok, fail } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.type === 'error') fail(new Error(`${msg.error}: ${msg.message}`));
        else ok(msg.result);
      }
    };
  }
  open() {
    return new Promise((ok, fail) => {
      this.ws.onopen = ok;
      this.ws.onerror = () => fail(new Error('BiDi connection failed'));
    });
  }
  send(method, params = {}) {
    const id = this.next++;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((ok, fail) => this.pending.set(id, { ok, fail }));
  }
  async evaluate(context, expression) {
    const result = await this.send('script.evaluate', {
      expression: `(async () => JSON.stringify(await (${expression})))()`,
      target: { context },
      awaitPromise: true,
      resultOwnership: 'none',
    });
    if (result.type === 'exception') throw new Error(`popup exception: ${result.exceptionDetails?.text}`);
    const value = result.result?.value;
    return value === undefined ? undefined : JSON.parse(value);
  }
  close() {
    this.ws.close();
  }
}

async function runFirefox(env, ctx) {
  const profile = ctx.firefoxProfile;
  mkdirSync(join(profile, 'extensions'), { recursive: true });
  buildXpi(ctx.extDir, join(profile, `extensions/${GECKO_ID}.xpi`));
  const prefs = {
    'xpinstall.signatures.required': false,
    'extensions.autoDisableScopes': 0,
    'extensions.enabledScopes': 15,
    'extensions.webextensions.uuids': JSON.stringify({ [GECKO_ID]: FIREFOX_UUID }),
    'browser.shell.checkDefaultBrowser': false,
    'datareporting.policy.dataSubmissionEnabled': false,
    'toolkit.telemetry.reportingpolicy.firstRun': false,
    'browser.startup.homepage_override.mstone': 'ignore',
    'browser.aboutwelcome.enabled': false,
    'app.update.disabledForTesting': true,
    'remote.active-protocols': 1,
  };
  writeFileSync(join(profile, 'user.js'), Object.entries(prefs).map(([k, v]) => `user_pref(${JSON.stringify(k)}, ${JSON.stringify(v)});\n`).join(''));
  const stderr = join(ctx.tmp, 'firefox.log');
  const child = spawn(ctx.executable, ['--no-remote', '--profile', profile, '--remote-debugging-port', '0', '--remote-allow-system-access', 'about:blank'], {
    env,
    stdio: ['ignore', 'ignore', openSync(stderr, 'w')],
    detached: true,
  });
  let wsUrl = null;
  await waitFor(() => {
    const m = /WebDriver BiDi listening on (ws:\/\/[^\s]+)/.exec(readFileSync(stderr, 'utf8'));
    wsUrl = m ? m[1] : null;
    return wsUrl !== null;
  }, 60000);
  if (!wsUrl) throw new Error(`Firefox did not start BiDi:\n${readFileSync(stderr, 'utf8').slice(-3000)}`);
  const bidi = new Bidi(`${wsUrl.replace(/\/$/, '')}/session`);
  await bidi.open();
  await bidi.send('session.new', { capabilities: {} });
  const tree = await bidi.send('browsingContext.getTree', {});
  const pageContext = tree.contexts[0].context;
  const browser = {
    pid: child.pid,
    close: async () => {
      try {
        await bidi.send('browser.close', {});
      } catch {
        // already closing
      }
      bidi.close();
      await waitFor(() => child.exitCode !== null || child.signalCode !== null, 20000);
      try {
        process.kill(-child.pid, 'SIGKILL');
      } catch {
        // gone
      }
    },
  };
  const popupUrl = `moz-extension://${FIREFOX_UUID}/popup.html`;
  const openPopup = async (pageUrl, title, { control = false } = {}) => {
    await bidi.send('browsingContext.navigate', { context: pageContext, url: pageUrl, wait: 'complete' });
    if (control) {
      const probe = (await bidi.send('browsingContext.create', { type: 'tab', background: true })).context;
      await bidi.send('browsingContext.navigate', { context: probe, url: popupUrl, wait: 'complete' });
      const tabs = await bidi.evaluate(probe, `browser.tabs.query({}).then((tabs) => tabs.map((t) => ({ id: t.id, url: t.url ?? null })))`);
      const selfId = await bidi.evaluate(probe, `browser.tabs.getCurrent().then((t) => t?.id ?? null)`);
      const target = tabs.find((t) => t.id !== selfId);
      await bidi.send('browsingContext.navigate', { context: probe, url: `${popupUrl}?tab=${target.id}`, wait: 'complete' });
      ctx.results.control = await negativeControl({ evaluate: (e) => bidi.evaluate(probe, e) }, tabs, pageUrl);
      await bidi.send('browsingContext.close', { context: probe });
    }
    await bidi.send('browsingContext.activate', { context: pageContext }).catch(() => undefined);
    await sleep(500);
    // 1. The real X key press (xdotool). Under Xvfb without a window manager Firefox does not
    //    receive X keyboard input, so 2. the same shortcut is delivered as a key event inside
    //    the browser window (privileged BiDi chrome scope). Either way it is Firefox's own
    //    shortcut -> _execute_action -> activeTab path that grants access to the tab.
    pressShortcut(title);
    await sleep(1000);
    const created = await bidi.send('browsingContext.create', { type: 'tab', background: true });
    const popup = created.context;
    await bidi.send('browsingContext.navigate', { context: popup, url: popupUrl, wait: 'complete' });
    const findTab = () => bidi.evaluate(popup, `browser.tabs.query({}).then((tabs) => tabs.find((t) => t.url === ${JSON.stringify(pageUrl)})?.id ?? null)`);
    let tabId = await findTab();
    let gesture = 'xdotool';
    if (tabId === null) {
      gesture = 'in-window trusted key event (nsITextInputProcessor)';
      const chromeWindow = (await bidi.send('browsingContext.getTree', { 'moz:scope': 'chrome' })).contexts[0].context;
      await bidi.send('browsingContext.activate', { context: pageContext }).catch(() => undefined);
      // A popup left open by the previous round would be toggled closed by the shortcut.
      await bidi.send('script.evaluate', {
        expression: `document.querySelectorAll('panel').forEach((p) => { if (p.state === 'open') p.hidePopup(); })`,
        target: { context: chromeWindow },
        awaitPromise: false,
      });
      await sleep(500);
      const fired = await bidi.send('script.evaluate', {
        // nsITextInputProcessor dispatches trusted key events through the widget-level path
        // (as EventUtils.synthesizeKey does), so XUL <key> shortcuts handle them.
        expression: `(() => {
          gURLBar.focus(); // the shortcut is handled by the browser window, not the page
          const tip = Cc['@mozilla.org/text-input-processor;1'].createInstance(Ci.nsITextInputProcessor);
          if (!tip.beginInputTransactionForTests(window)) return 'no input transaction';
          const alt = new KeyboardEvent('', { key: 'Alt', code: 'AltLeft' });
          const shift = new KeyboardEvent('', { key: 'Shift', code: 'ShiftLeft' });
          const s = new KeyboardEvent('', { key: 'S', code: 'KeyS', keyCode: 83 });
          tip.keydown(alt);
          tip.keydown(shift);
          tip.keydown(s);
          tip.keyup(s);
          tip.keyup(shift);
          tip.keyup(alt);
          return [...document.querySelectorAll('keyset[id^="ext-keyset"] key')].map((k) => k.getAttribute('modifiers') + '+' + k.getAttribute('key')).join();
        })()`,
        target: { context: chromeWindow },
        awaitPromise: false,
      });
      ctx.results.firefox_registered_shortcut = fired.result?.value;
      await sleep(1000);
      tabId = await findTab();
    }
    ctx.results.firefox_gesture = gesture;
    if (tabId === null) throw new Error('fixture tab not visible to the extension: activeTab was not granted');
    await bidi.send('browsingContext.navigate', { context: popup, url: `${popupUrl}?tab=${tabId}`, wait: 'complete' });
    return {
      mode: 'popup-in-tab',
      evaluate: (expression) => bidi.evaluate(popup, expression),
      close: () => bidi.send('browsingContext.close', { context: popup }).catch(() => undefined),
    };
  };
  return { browser, openPopup };
}

// ---- main --------------------------------------------------------------------------------

async function main() {
  const args = parseArgs();
  if (!process.env.DISPLAY) throw new Error('DISPLAY is not set (start Xvfb, e.g. Xvfb :99 &; export DISPLAY=:99)');
  const tmp = mkdtempSync(join(tmpdir(), `cm-ext-e2e-${args.browser}-`));
  const checks = {};
  const results = { browser: args.browser };
  const prefixBin = join(tmp, 'prefix/bin');
  const papersd = join(prefixBin, 'papersd');
  const host = join(prefixBin, 'papers-native-host');
  let browser = null;
  let server = null;
  try {
    mkdirSync(prefixBin, { recursive: true });
    for (const name of ['papersd', 'papers-native-host']) {
      copyFileSync(join(args.binDir, name), join(prefixBin, name));
      chmodSync(join(prefixBin, name), 0o755);
    }
    const library = join(tmp, 'library');
    const runtime = join(tmp, 'run');
    mkdirSync(runtime, { mode: 0o700 });
    mkdirSync(join(tmp, 'home'));
    const env = {
      PATH: process.env.PATH,
      DISPLAY: process.env.DISPLAY,
      LANG: process.env.LANG ?? 'C.UTF-8',
      HOME: join(tmp, 'home'),
      XDG_CONFIG_HOME: join(tmp, 'config'),
      XDG_RUNTIME_DIR: runtime,
      TMPDIR: tmp,
    };
    if (process.env.XAUTHORITY) env.XAUTHORITY = process.env.XAUTHORITY;

    // The built extension (development IDs).
    const extDir = join(tmp, 'extension');
    cpSync(join(extRoot, 'dist', args.browser === 'firefox' ? 'firefox' : 'chrome'), extDir, { recursive: true });
    const chromiumProfile = join(tmp, 'chromium-profile');
    const firefoxProfile = join(tmp, 'firefox-profile');
    const manifestSpec = args.browser === 'firefox' ? 'firefox' : `chromium=${join(chromiumProfile, 'NativeMessagingHosts')}`;
    const register = spawnSync(host, ['register', '--library', library, '--runtime-dir', runtime, '--browser', manifestSpec, '--development-extensions', '--pair'], { env, encoding: 'utf8', timeout: 90000 });
    log(`register --pair: exit ${register.status}\n${register.stdout}${register.stderr}`);
    checks.register_and_pair = register.status === 0;
    if (!checks.register_and_pair) throw new Error('register failed');

    log('waiting for the pairing papersd to idle out');
    checks.papersd_stopped_before_browser = await waitFor(() => processesRunning(papersd).length === 0, 90000, 500);

    server = await serveFixtures();
    const base = `http://127.0.0.1:${server.address().port}`;
    const ctx = { tmp, extDir, chromiumProfile, firefoxProfile, executable: args.executable, checks, results };
    const driver = args.browser === 'firefox' ? await runFirefox(env, ctx) : await runChromium(env, ctx);
    browser = driver.browser;

    let popup = await driver.openPopup(`${base}/article.html`, 'Capturing Papers With the Desktop App Closed', { control: true });
    results.popup_mode = popup.mode;
    checks.without_gesture_tab_url_hidden = results.control?.tab_urls_visible === 0;
    checks.without_gesture_save_has_no_access = results.control?.save_error === 'no_access';
    await articleScenario(popup, checks, results);
    await popup.close();

    popup = await driver.openPopup(`${base}/article.html`, 'Capturing Papers With the Desktop App Closed');
    await duplicateScenario(popup, checks, results);
    await popup.close();

    popup = await driver.openPopup(`${base}/no-metadata.html`, 'Lab notes');
    await provisionalScenario(popup, checks, results);
    await popup.close();

    checks.host_exited_after_requests = await waitFor(() => processesRunning(host).length === 0, 15000);
    await browser.close();
    browser = null;
    const daemons = processesRunning(papersd);
    results.papersd_after_browser_quit = daemons;
    log(`browser closed; papersd pids ${JSON.stringify(daemons)}, host pids ${JSON.stringify(processesRunning(host))}`);

    // "Launch the app": open the library as its owner, like the desktop app does.
    for (const [key, paper] of [['article', results.article_paper], ['provisional', results.provisional_paper]]) {
      if (!paper) continue;
      const inspect = spawnSync(join(args.binDir, 'examples/inspect_capture'), ['--library', library, '--runtime-dir', runtime, '--papersd', papersd, '--paper', paper, '--wait-secs', '5'], { env, encoding: 'utf8', timeout: 120000 });
      log(`inspect ${key}: ${inspect.stdout}${inspect.stderr}`);
      const state = JSON.parse(inspect.stdout || '{}');
      results[`${key}_persisted`] = state.paper;
      checks[`${key}_persisted_after_browser_quit`] = state.paper?.found === true && state.paper?.deleted === false;
      if (key === 'provisional') checks.provisional_persisted_with_correction = state.paper?.title === CORRECTED_TITLE && state.paper?.year === 2025;
      if (key === 'article') checks.article_persisted_with_doi = JSON.stringify(state.paper?.external_ids ?? []).includes('10.9999/e2e24.capture');
    }
  } catch (error) {
    checks.no_exception = false;
    results.error = String(error?.stack ?? error);
    log(`error: ${results.error}`);
  } finally {
    if (browser) await browser.close().catch(() => undefined);
    server?.close();
    for (const pid of processesRunning(papersd)) {
      try {
        process.kill(pid, 'SIGTERM');
      } catch {
        // gone
      }
    }
    if (args.keep) log(`kept ${tmp}`);
    else rmSync(tmp, { recursive: true, force: true });
  }
  log(`results: ${JSON.stringify(results, null, 2)}`);
  log(`checks: ${JSON.stringify(checks, null, 2)}`);
  if (args.report) writeFileSync(args.report, JSON.stringify({ checks, results }, null, 2));
  const passed = Object.keys(checks).length > 0 && Object.values(checks).every(Boolean);
  log(passed ? 'PASSED' : 'FAILED');
  process.exit(passed ? 0 : 1);
}

if (!existsSync(join(extRoot, 'dist/chrome/manifest.json'))) {
  console.error('build first: npm run build');
  process.exit(2);
}
await main();
