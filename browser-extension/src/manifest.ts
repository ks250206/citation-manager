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

export function buildManifest(target: Target, channel: Channel, version: string, ids: ExtensionIds): Record<string, unknown> {
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
    content_security_policy: { extension_pages: "script-src 'self'; object-src 'none'; base-uri 'none'" },
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
      strict_min_version: '128.0',
      // Page data goes only to the local native host, never to the developer or a server.
      data_collection_permissions: { required: ['none'] },
    },
  };
  return base;
}
