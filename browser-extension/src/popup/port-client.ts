// Popup side of the `capture-popup` port.
//
// - Every request has a deadline (a little longer than the background's own host deadline),
//   so the popup is never stuck.
// - While the popup is open it pings the background every 20 s: a Chrome MV3 service worker
//   is otherwise stopped when idle, which would drop the port (and its open preview).
// - If the port is lost anyway (worker restarted), pending requests fail and the next request
//   reconnects. A preview held by the lost session is unknown to the new one: committing it
//   answers `not_found`, which the popup turns into "Preview again". The daemon expires the
//   orphaned preview (≤ 300 s). A commit that was in flight is reported as of unknown outcome.
import type { Port } from '../shared/api';
import type { PopupReply } from '../shared/messages';
import type { Outcome } from '../shared/types';
import type { Client } from './controller';

export const PING_MS = 20_000;
/** Background host deadlines are ≤ 30 s; leave room for extraction and validation. */
export const REQUEST_DEADLINE_MS = 45_000;
export const COMMIT_UNKNOWN_MESSAGE =
  'The connection to the extension was lost while saving. The save may have completed: check the app before saving again.';

export interface PortClientOptions {
  connect(): Port;
  setTimer?: (fn: () => void, ms: number) => unknown;
  clearTimer?: (handle: unknown) => void;
  setRepeat?: (fn: () => void, ms: number) => unknown;
  deadlineMs?: number;
  pingMs?: number;
}

interface Pending {
  op: string;
  resolve(outcome: Outcome<unknown>): void;
  timer: unknown;
}

function lost(op: string, code: string, message: string): Outcome<unknown> {
  if (op === 'commit') return { ok: false, error: { code: 'commit_unknown', message: COMMIT_UNKNOWN_MESSAGE, retryable: false } };
  return { ok: false, error: { code, message, retryable: true } };
}

export function portClient(options: PortClientOptions): Client & { close(): void } {
  const setTimer = options.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
  const clearTimer = options.clearTimer ?? ((h) => clearTimeout(h as ReturnType<typeof setTimeout>));
  const setRepeat = options.setRepeat ?? ((fn, ms) => setInterval(fn, ms));
  const deadline = options.deadlineMs ?? REQUEST_DEADLINE_MS;
  const pending = new Map<number, Pending>();
  let port: Port | null = null;
  let next = 1;

  const settle = (rid: number, outcome: Outcome<unknown>) => {
    const p = pending.get(rid);
    if (!p) return;
    pending.delete(rid);
    clearTimer(p.timer);
    p.resolve(outcome);
  };

  const ensure = (): Port => {
    if (port) return port;
    const fresh = options.connect();
    fresh.onMessage.addListener((message) => {
      const reply = message as PopupReply;
      if (typeof reply !== 'object' || reply === null || typeof reply.rid !== 'number') return;
      settle(reply.rid, reply.outcome);
    });
    fresh.onDisconnect.addListener(() => {
      if (port === fresh) port = null;
      for (const [rid, p] of [...pending]) {
        settle(rid, lost(p.op, 'background_gone', 'The extension restarted. Try again.'));
      }
    });
    port = fresh;
    return fresh;
  };

  const request: Client['request'] = (body) => {
    const rid = next++;
    return new Promise((resolve) => {
      const timer = setTimer(() => settle(rid, lost(body.op, 'timeout', 'The extension did not answer in time. Try again.')), deadline);
      pending.set(rid, { op: body.op, resolve, timer });
      try {
        ensure().postMessage({ ...body, rid });
      } catch {
        port = null;
        settle(rid, lost(body.op, 'background_gone', 'The extension restarted. Try again.'));
      }
    });
  };

  const pinger = setRepeat(() => {
    void request({ op: 'ping' });
  }, options.pingMs ?? PING_MS);

  return {
    request,
    close() {
      clearInterval(pinger as ReturnType<typeof setInterval>);
      port?.disconnect();
      port = null;
    },
  };
}
