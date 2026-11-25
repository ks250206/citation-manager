// One manifest source, two browser outputs (MV3 for both). Used by build.mjs and tested.
//
// Permissions are the minimum for an explicit-save flow:
//  - activeTab: read the tab only after the user invokes the action (toolbar or shortcut);
//  - scripting: inject the extractor into that tab at that moment;
//  - nativeMessaging: talk to papers-native-host;
//  - storage: remember the last destination and offline choice.
// There are no host permissions, no content_scripts, no web_accessible_resources and no
// externally_connectable entry: pages cannot reach the extension and nothing runs on a page
// the user did not save.

export type Target = 'chrome' | 'firefox';
export type Channel = 'development' | 'release';

export interface ExtensionIds {
  development: { chrome: string[]; firefox: string[] };
  release: { chrome: string[]; firefox: string[] };
}

/** Public key of the pinned development Chrome ID (browser-extension/native-manifests). */
export const DEV_CHROME_KEY =
  'MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAm1S3fbpH7Q2acvaU7O9NMu12EoGL72t39Mefcz++wJz4NjC52730oDgR17NvwPoOFvp53UnQcxN3V7ISnKXV99h/OFu4j8gEut9APRfu26+pwUQsp+dXmdV7+Fg+2C3H2p90ZyUnRYY0vaX/lvcKbvJTGFcmnSBZ/Gz8Di9j8FzfK1IRWp2ix6vkugdFa8sjeKi+SSwGANnCyRazI4jqBiS1wxlXiHqj2kBA11CwaKcMsf42lEGGUCnF515tcSs/2AQp16CBvpyGdHue3LsSIuZkIEeD8kpjjI0u2CL3TMHxbYypvTO1thYt/95s234VTZIdV9lPGIa6wF9+HFTvAwIDAQAB';

export const PERMISSIONS = ['activeTab', 'scripting', 'nativeMessaging', 'storage'] as const;

/** Chrome extension IDs: 32 letters a-p (derived from the public key). */
export const CHROME_ID = /^[a-p]{32}$/;
/** Gecko add-on IDs: email-like `name@domain` or a `{UUID}` (as papers-native-host accepts). */
export const GECKO_ID = /^(?:[A-Za-z0-9._+-]+@[A-Za-z0-9.-]+\.[A-Za-z0-9-]+|\{[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\})$/;

/** Rejects malformed IDs, empty strings, and development IDs listed as release IDs. */
export function checkIds(ids: ExtensionIds): void {
  const fail = (m: string): never => {
    throw new Error(`native-manifests/extension-ids.json: ${m}`);
  };
  for (const channel of ['development', 'release'] as const) {
    const list = ids[channel];
    if (!list || !Array.isArray(list.chrome) || !Array.isArray(list.firefox)) fail(`${channel} needs chrome and firefox lists`);
    for (const id of list.chrome) if (typeof id !== 'string' || !CHROME_ID.test(id)) fail(`bad ${channel} Chrome ID ${JSON.stringify(id)}`);
    for (const id of list.firefox) {
      if (typeof id !== 'string' || id.length > 80 || !GECKO_ID.test(id)) fail(`bad ${channel} Firefox ID ${JSON.stringify(id)}`);
    }
  }
  for (const id of [...ids.release.chrome, ...ids.release.firefox]) {
    if (ids.development.chrome.includes(id) || ids.development.firefox.includes(id)) fail(`development ID ${id} listed as release`);
  }
}

export function buildManifest(target: Target, channel: Channel, version: string, ids: ExtensionIds): Record<string, unknown> {
  checkIds(ids);
  const base: Record<string, unknown> = {
    manifest_version: 3,
    name: channel === 'development' ? 'Citation Manager Capture (development)' : 'Citation Manager Capture',
    short_name: 'Citation Manager',
    description: 'Save the paper on the current page to your local Citation Manager library.',
    version,
    permissions: [...PERMISSIONS],
    action: {
      default_title: 'Save to Citation Manager',
      default_popup: 'popup.html',
      default_icon: { '16': 'icons/icon-16.png', '32': 'icons/icon-32.png' },
    },
    icons: { '48': 'icons/icon-48.png', '128': 'icons/icon-128.png' },
    // The keyboard shortcut is an explicit user action too (it grants activeTab).
    commands: {
      _execute_action: {
        suggested_key: { default: 'Alt+Shift+S' },
        description: 'Save the current page to Citation Manager',
      },
    },
    // Extension pages never fetch or submit anything: host traffic is Native Messaging only.
    content_security_policy: {
      extension_pages: "script-src 'self'; object-src 'none'; base-uri 'none'; connect-src 'none'; form-action 'none'",
    },
  };
  if (target === 'chrome') {
    base.background = { service_worker: 'background.js' };
    base.minimum_chrome_version = '116';
    if (channel === 'development') {
      // Fixes the development ID baapgkfjfjjagdkpkmocjhfopmaohcdc allowed by the native host.
      base.key = DEV_CHROME_KEY;
    } else if (ids.release.chrome.length === 0) {
      throw new Error('release build: no Chrome Web Store ID in native-manifests/extension-ids.json');
    }
    return base;
  }
  const gecko = channel === 'development' ? ids.development.firefox[0] : ids.release.firefox[0];
  if (!gecko) throw new Error(`${channel} build: no Firefox add-on ID in native-manifests/extension-ids.json`);
  base.background = { scripts: ['background.js'] };
  base.browser_specific_settings = {
    gecko: {
      id: gecko,
      // data_collection_permissions is understood from Firefox 140 (ESR) on.
      strict_min_version: '140.0',
      // Page data goes only to the local native host, never to the developer or a server.
      data_collection_permissions: { required: ['none'] },
    },
  };
  return base;
}
