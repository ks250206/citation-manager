// Native host client: one-shot `runtime.sendNativeMessage` per request (the host exits after
// each one, so nothing keeps papersd alive). Every request is size-checked before sending and
// every reply is validated against the v1 contract.
import { HOST_NAME, PROTOCOL_VERSION } from '../shared/limits';
import type { ErrorInfo, Outcome } from '../shared/types';
import { BoundaryError, checkFrameSize, checkReply } from '../shared/validate';

export type NativeSend = (host: string, message: unknown) => Promise<unknown>;

/** Maps browser-side native messaging failures (no reply at all) to stable codes. */
export function nativeError(error: unknown): ErrorInfo {
  const message = error instanceof Error ? error.message : String(error);
  if (/not found|no such native application|manifest/i.test(message)) {
    return { code: 'host_missing', message: 'The Citation Manager native host is not installed for this browser.', retryable: false };
  }
  if (/forbidden|not allowed/i.test(message)) {
    return { code: 'host_forbidden', message: 'The native host does not allow this extension ID.', retryable: false };
  }
  return { code: 'host_failed', message: `The native host stopped unexpectedly (${message.slice(0, 200)}).`, retryable: true };
}

/**
 * Per-type reply deadlines. The host itself bounds daemon start (15 s) and replies (300 s);
 * the extension gives up earlier so the popup is never stuck. A late reply is handed to
 * `onLate` (the session cancels a late preview).
 */
export const DEADLINES_MS: Record<string, number> = {
  hello: 10_000,
  destinations: 15_000,
  'capture.preview': 30_000,
  'capture.commit': 30_000,
  'capture.cancel': 15_000,
  'capture.pdf_job': 15_000,
  'capture.retry_pdf': 15_000,
};

export const TIMEOUT_MESSAGE = 'Citation Manager did not answer in time. Try again.';

const TIMEOUT = Symbol('timeout');

export class HostClient {
  private sequence = 0;
  private readonly prefix = Date.now().toString(36);

  constructor(
    private readonly send: NativeSend,
    private readonly deadlines: Record<string, number> = DEADLINES_MS,
  ) {}

  async call<T>(
    type: string,
    fields: Record<string, unknown>,
    parse: (v: unknown) => T,
    onLate?: (outcome: Outcome<T>) => void,
  ): Promise<Outcome<T>> {
    const id = `cm-${this.prefix}-${++this.sequence}`;
    const message = { ...fields, v: PROTOCOL_VERSION, id, type };
    try {
      checkFrameSize(message);
    } catch (error) {
      return { ok: false, error: (error as BoundaryError).info() };
    }
    const settle = (raw: unknown): Outcome<T> => {
      try {
        return checkReply(raw, id, type, parse);
      } catch (error) {
        if (error instanceof BoundaryError) return { ok: false, error: error.info() };
        throw error;
      }
    };
    const sent = this.send(HOST_NAME, message);
    let timer: ReturnType<typeof setTimeout> | undefined;
    const deadline = new Promise<typeof TIMEOUT>((resolve) => {
      timer = setTimeout(() => resolve(TIMEOUT), this.deadlines[type] ?? 30_000);
    });
    let raw: unknown;
    try {
      raw = await Promise.race([sent, deadline]);
    } catch (error) {
      return { ok: false, error: nativeError(error) };
    } finally {
      clearTimeout(timer);
    }
    if (raw === TIMEOUT) {
      sent.then(
        (late) => onLate?.(settle(late)),
        () => undefined,
      );
      return { ok: false, error: { code: 'timeout', message: TIMEOUT_MESSAGE, retryable: true } };
    }
    return settle(raw);
  }
}
