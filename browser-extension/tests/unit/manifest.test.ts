// Manifest generation: one source, MV3 for both browsers, minimal permissions, pinned IDs.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { test } from 'node:test';
import { buildManifest, DEV_CHROME_KEY, type ExtensionIds } from '../../src/manifest';
import { HOST_NAME } from '../../src/shared/limits';
import { REPO_ROOT } from './helpers';

const ids = JSON.parse(readFileSync(join(REPO_ROOT, 'browser-extension/native-manifests/extension-ids.json'), 'utf8')) as ExtensionIds;

/** Chrome derives the extension ID from the SHA-256 of the DER public key (a-p alphabet). */
function chromeId(base64Key: string): string {
  const hex = createHash('sha256').update(Buffer.from(base64Key, 'base64')).digest('hex').slice(0, 32);
  return [...hex].map((c) => String.fromCharCode('a'.charCodeAt(0) + parseInt(c, 16))).join('');
}

test('Chrome: MV3 service worker, development key yields the allowed development ID', () => {
  const m = buildManifest('chrome', 'development', '0.1.0', ids);
  assert.equal(m.manifest_version, 3);
  assert.deepEqual(m.background, { service_worker: 'background.js' });
  assert.equal(m.key, DEV_CHROME_KEY);
  assert.equal(chromeId(DEV_CHROME_KEY), ids.development.chrome[0]);
  assert.equal(chromeId(DEV_CHROME_KEY), 'baapgkfjfjjagdkpkmocjhfopmaohcdc');
  // Same key as the #23 smoke extension.
  const smoke = JSON.parse(readFileSync(join(REPO_ROOT, 'browser-extension/native-manifests/dev-extension/manifest.chrome.json'), 'utf8'));
  assert.equal(smoke.key, DEV_CHROME_KEY);
});

test('Firefox: MV3 background scripts with the gecko ID', () => {
  const m = buildManifest('firefox', 'development', '0.1.0', ids);
  assert.equal(m.manifest_version, 3);
  assert.deepEqual(m.background, { scripts: ['background.js'] });
  const gecko = (m.browser_specific_settings as { gecko: { id: string } }).gecko;
  assert.equal(gecko.id, 'citation-manager-dev@citation-manager.invalid');
  assert.ok(!('key' in m));
});

test('both: only activeTab/scripting/nativeMessaging/storage; no page access without a click', () => {
  for (const target of ['chrome', 'firefox'] as const) {
    const m = buildManifest(target, 'development', '0.1.0', ids);
    assert.deepEqual(m.permissions, ['activeTab', 'scripting', 'nativeMessaging', 'storage']);
    for (const key of ['host_permissions', 'optional_host_permissions', 'content_scripts', 'web_accessible_resources', 'externally_connectable', 'optional_permissions']) {
      assert.ok(!(key in m), `${target}: ${key}`);
    }
    assert.doesNotMatch(JSON.stringify(m), /<all_urls>|\*:\/\//);
    assert.equal((m.action as { default_popup: string }).default_popup, 'popup.html');
    assert.ok((m.commands as Record<string, unknown>)._execute_action);
  }
});

test('release builds need the store IDs (placeholders are empty today)', () => {
  assert.deepEqual(ids.release, { chrome: [], firefox: [] });
  assert.throws(() => buildManifest('chrome', 'release', '0.1.0', ids), /Chrome Web Store ID/);
  assert.throws(() => buildManifest('firefox', 'release', '0.1.0', ids), /Firefox add-on ID/);
  const filled: ExtensionIds = { ...ids, release: { chrome: ['abcdefghijklmnopabcdefghijklmnop'], firefox: ['capture@example.org'] } };
  const chrome = buildManifest('chrome', 'release', '0.1.0', filled);
  assert.ok(!('key' in chrome), 'store builds carry no development key');
  assert.equal((buildManifest('firefox', 'release', '0.1.0', filled).browser_specific_settings as { gecko: { id: string } }).gecko.id, 'capture@example.org');
});

test('the host name matches the native host registration', () => {
  assert.equal(HOST_NAME, 'dev.citation_manager.native_host');
  const source = readFileSync(join(REPO_ROOT, 'crates/native-host/src/manifest.rs'), 'utf8');
  assert.ok(source.includes(HOST_NAME));
});
