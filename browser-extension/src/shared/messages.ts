// Popup <-> background messages over the `capture-popup` port. The background is the only
// context that talks to the native host; it re-validates everything the popup sends.
import { BoundaryError, checkCaptureRequest, isUuid } from './validate';
import type { CaptureRequest, Decision, ErrorInfo, Outcome } from './types';

export const PORT_NAME = 'capture-popup';

export type PopupRequest =
  | { rid: number; op: 'hello' }
  | { rid: number; op: 'destinations' }
  | { rid: number; op: 'extract'; tabId: number }
  | { rid: number; op: 'preview'; capture: CaptureRequest }
  | { rid: number; op: 'commit'; preview_id: string; decisions: Decision[]; pdf: number | null }
  | { rid: number; op: 'cancel'; preview_id: string }
  | { rid: number; op: 'pdf_job'; job: string }
  | { rid: number; op: 'retry_pdf'; job: string };

export type Op = PopupRequest['op'];

export interface PopupReply {
  rid: number;
  outcome: Outcome<unknown>;
}

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

function reject(message: string): never {
  throw new BoundaryError('bad_request', message);
}

/** Shape check of a popup request; content limits are checked again by the session. */
export function parsePopupRequest(raw: unknown): PopupRequest {
  if (!isObject(raw)) reject('request must be an object');
  const rid = raw.rid;
  if (typeof rid !== 'number' || !Number.isSafeInteger(rid) || rid < 0) reject('rid');
  const previewId = (v: unknown): string => {
    if (typeof v !== 'string' || v.length === 0 || v.length > 256) reject('preview_id');
    return v;
  };
  switch (raw.op) {
    case 'hello':
    case 'destinations':
      return { rid, op: raw.op };
    case 'extract':
      if (typeof raw.tabId !== 'number' || !Number.isSafeInteger(raw.tabId) || raw.tabId < 0) reject('tabId');
      return { rid, op: 'extract', tabId: raw.tabId };
    case 'preview':
      return { rid, op: 'preview', capture: checkCaptureRequest(raw.capture) };
    case 'commit': {
      if (!Array.isArray(raw.decisions)) reject('decisions');
      const pdf = raw.pdf ?? null;
      if (pdf !== null && (typeof pdf !== 'number' || !Number.isInteger(pdf) || pdf < 0)) reject('pdf');
      return { rid, op: 'commit', preview_id: previewId(raw.preview_id), decisions: raw.decisions as Decision[], pdf };
    }
    case 'cancel':
      return { rid, op: 'cancel', preview_id: previewId(raw.preview_id) };
    case 'pdf_job':
    case 'retry_pdf':
      if (!isUuid(raw.job)) reject('job');
      return { rid, op: raw.op, job: raw.job };
    default:
      return reject('unknown op');
  }
}

export function errorInfo(error: unknown): ErrorInfo {
  if (error instanceof BoundaryError) return error.info();
  return { code: 'internal', message: error instanceof Error ? error.message : String(error), retryable: false };
}
