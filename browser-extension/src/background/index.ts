// Background (Chrome: MV3 service worker; Firefox: MV3 background script). The only context
// that injects the extractor and talks to papers-native-host. It listens only for the
// popup's `capture-popup` port: no runtime.onMessage, no external messaging, no web
// accessible resources, so neither web pages nor content scripts can reach the host.
import { webext } from '../shared/api';
import { PORT_NAME } from '../shared/messages';
import { BoundaryError } from '../shared/validate';
import { checkSnapshot, isPopupSender } from './boundary';
import { HostClient } from './host';
import { CaptureSession } from './session';

const api = webext();
const popupUrl = api.runtime.getURL('popup.html');
const host = new HostClient((name, message) => api.runtime.sendNativeMessage(name, message));

async function extract(tabId: number) {
  // activeTab: the tab URL and script access exist only after the user invoked the action
  // on this tab. Without that grant both calls fail and nothing is read.
  const tab = await api.tabs.get(tabId);
  const results = await api.scripting.executeScript({ target: { tabId, frameIds: [0] }, files: ['content.js'] });
  return checkSnapshot(results, tab);
}

api.runtime.onConnect.addListener((port) => {
  if (port.name !== PORT_NAME || !isPopupSender(port.sender, api.runtime.id, popupUrl)) {
    port.disconnect();
    return;
  }
  const session = new CaptureSession({
    host,
    extract: async (tabId) => {
      try {
        return await extract(tabId);
      } catch (error) {
        if (error instanceof BoundaryError) throw error;
        // No activeTab grant, a privileged page (browser settings, the add-on store), a
        // closed tab, or a page that blocked injection.
        throw new BoundaryError('no_access', 'Cannot read this page. Open the article page and use the toolbar button.');
      }
    },
  });
  port.onMessage.addListener((message) => {
    void session.handle(message).then((reply) => {
      try {
        port.postMessage(reply);
      } catch {
        // The popup closed while the host was answering.
      }
    });
  });
  port.onDisconnect.addListener(() => {
    void session.close();
  });
});
