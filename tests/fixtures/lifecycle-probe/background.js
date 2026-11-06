/* Test-only. Only nativeMessaging; no network, page access, paths or user data. */
const api = globalThis.browser ?? chrome;
const ports = new Set();
let reconnect = true;
function openProbe() {
  const port = api.runtime.connectNative(CONFIG.host);
  ports.add(port);
  let acknowledged = false;
  port.onMessage.addListener(reply => {
    if (!acknowledged && reply.nonce === CONFIG.nonce) {
      acknowledged = true;
      // Echo the *actual received* PIDs. Host validates them and records client-ack.
      port.postMessage({nonce: CONFIG.nonce, op: "ack", host_pid: reply.host_pid,
                        daemon_pid: reply.daemon_pid});
    }
  });
  const timer = setInterval(() => {
    try { port.postMessage({nonce: CONFIG.nonce, op: "ping"}); } catch (_) {}
  }, 400);
  port.onDisconnect.addListener(() => {
    clearInterval(timer);
    ports.delete(port);
    // Read lastError for Chrome; no exception leakage from normal teardown.
    void api.runtime.lastError;
    if (reconnect) setTimeout(openProbe, 300);
  });
  port.postMessage({nonce: CONFIG.nonce, op: "ping"});
}
openProbe();
openProbe();
// Actual disconnect closes native ports, letting browsers apply their normal cleanup.
setTimeout(() => { reconnect = false; for (const p of ports) p.disconnect(); ports.clear(); }, 7000);
// >2 s idle timeout: next connect must lazy-start a new daemon instance.
setTimeout(() => { reconnect = true; openProbe(); }, 11000);
