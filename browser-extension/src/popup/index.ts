// Popup entry. Opens the `capture-popup` port to the background and drives the controller.
// Closing the popup closes the port; the background then cancels an uncommitted preview.
import { webext } from '../shared/api';
import { PORT_NAME, type PopupReply } from '../shared/messages';
import type { Destination, Outcome } from '../shared/types';
import { isUuid } from '../shared/validate';
import { Controller, type Client } from './controller';
import { initialState } from './state';
import { render } from './view';

const api = webext();
const STORAGE_KEY = 'capture.preferences';

function portClient(): Client {
  const port = api.runtime.connect({ name: PORT_NAME });
  const pending = new Map<number, (outcome: Outcome<unknown>) => void>();
  let next = 1;
  let disconnected = false;
  port.onMessage.addListener((message) => {
    const reply = message as PopupReply;
    if (typeof reply !== 'object' || reply === null || typeof reply.rid !== 'number') return;
    const resolve = pending.get(reply.rid);
    if (!resolve) return;
    pending.delete(reply.rid);
    resolve(reply.outcome);
  });
  port.onDisconnect.addListener(() => {
    disconnected = true;
    for (const resolve of pending.values()) {
      resolve({ ok: false, error: { code: 'background_gone', message: 'The extension was restarted; reopen the popup.', retryable: false } });
    }
    pending.clear();
  });
  return {
    request(body) {
      if (disconnected) {
        return Promise.resolve({ ok: false, error: { code: 'background_gone', message: 'Reopen the popup.', retryable: false } });
      }
      const rid = next++;
      return new Promise((resolve) => {
        pending.set(rid, resolve);
        port.postMessage({ ...body, rid });
      });
    },
  };
}

/** `?tab=<id>` (popup opened in a tab, e.g. by tests) or the active tab of this window. */
async function targetTab(): Promise<number> {
  const param = new URLSearchParams(location.search).get('tab');
  if (param !== null) {
    const id = Number(param);
    if (!Number.isSafeInteger(id) || id < 0) throw new Error('bad tab');
    return id;
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
      client: portClient(),
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
