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

export class HostClient {
  private sequence = 0;
  private readonly prefix = Date.now().toString(36);

  constructor(private readonly send: NativeSend) {}

  async call<T>(type: string, fields: Record<string, unknown>, parse: (v: unknown) => T): Promise<Outcome<T>> {
    const id = `cm-${this.prefix}-${++this.sequence}`;
    const message = { ...fields, v: PROTOCOL_VERSION, id, type };
    try {
      checkFrameSize(message);
    } catch (error) {
      return { ok: false, error: (error as BoundaryError).info() };
    }
    let raw: unknown;
    try {
      raw = await this.send(HOST_NAME, message);
    } catch (error) {
      return { ok: false, error: nativeError(error) };
    }
    try {
      return checkReply(raw, id, type, parse);
    } catch (error) {
      if (error instanceof BoundaryError) return { ok: false, error: error.info() };
      throw error;
    }
  }
}
