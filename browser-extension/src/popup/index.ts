// Popup entry. Opens the `capture-popup` port to the background and drives the controller.
// Closing the popup closes the port; the background then cancels an uncommitted preview.
import { webext } from '../shared/api';
import { PORT_NAME } from '../shared/messages';
import type { Destination } from '../shared/types';
import { isUuid } from '../shared/validate';
import { Controller } from './controller';
import { portClient } from './port-client';
import { initialState } from './state';
import { render } from './view';

const api = webext();
const STORAGE_KEY = 'capture.preferences';

/**
 * The active tab of this window. Development/E2E builds also accept `?tab=<id>` (the popup
 * document opened in a tab by the E2E driver); release builds compile that hook out.
 */
async function targetTab(): Promise<number> {
  if (__TEST_HOOKS__) {
    const param = new URLSearchParams(location.search).get('tab');
    if (param !== null) {
      const id = Number(param);
      if (!Number.isSafeInteger(id) || id < 0) throw new Error('bad tab');
      return id;
    }
  }
  const [tab] = await api.tabs.query({ active: true, currentWindow: true });
  if (tab?.id === undefined) throw new Error('no active tab');
  return tab.id;
}

async function preferences(): Promise<{ destination: Destination; offline: boolean }> {
  const fallback = { destination: { collection: null, tags: [] }, offline: false };
  try {
    const stored = (await api.storage?.local.get([STORAGE_KEY]))?.[STORAGE_KEY] as
      | { collection?: unknown; tags?: unknown; offline?: unknown }
      | undefined;
    if (!stored) return fallback;
    return {
      destination: {
        collection: isUuid(stored.collection) ? stored.collection : null,
        tags: Array.isArray(stored.tags) ? stored.tags.filter(isUuid).slice(0, 256) : [],
      },
      offline: stored.offline === true,
    };
  } catch {
    return fallback;
  }
}

async function main(): Promise<void> {
  const root = document.getElementById('app');
  if (!root) return;
  const prefs = await preferences();
  const controller = new Controller(
    {
      client: portClient({ connect: () => api.runtime.connect({ name: PORT_NAME }) }),
      tabId: targetTab,
      sleep: (ms) => new Promise((resolve) => setTimeout(resolve, ms)),
      remember: (destination, offline) => {
        api.storage?.local.set({ [STORAGE_KEY]: { ...destination, offline } }).catch(() => undefined);
      },
    },
    initialState(prefs.destination, prefs.offline),
  );
  controller.subscribe((state) => render(root, state, controller));
  render(root, controller.state, controller);
  await controller.start();
}

void main();
