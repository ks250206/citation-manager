// One popup connection. Translates popup requests into host messages, after validating them
// against what this session itself received from the host (decision and PDF indices refer to
// its own previews). Superseded and abandoned previews are cancelled so they do not occupy
// the daemon's small preview capacity.
import { MAX_PDF_CANDIDATES } from '../shared/limits';
import { errorInfo, parsePopupRequest, type PopupReply, type PopupRequest } from '../shared/messages';
import type { Outcome, PageSnapshot, PreviewResult } from '../shared/types';
import {
  BoundaryError,
  checkDecisions,
  parseCancel,
  parseCommit,
  parseDestinations,
  parseHello,
  parseJob,
  parsePreview,
} from '../shared/validate';
import type { HostClient } from './host';

export interface SessionDeps {
  host: HostClient;
  /** Injects the extractor into `tabId` (top frame) and returns the validated snapshot. */
  extract(tabId: number): Promise<PageSnapshot>;
}

interface OpenPreview {
  candidates: number;
  pdfs: number;
}

export class CaptureSession {
  private readonly previews = new Map<string, OpenPreview>();
  private closed = false;

  constructor(private readonly deps: SessionDeps) {}

  async handle(raw: unknown): Promise<PopupReply> {
    const rid =
      typeof raw === 'object' && raw !== null && typeof (raw as { rid?: unknown }).rid === 'number'
        ? (raw as { rid: number }).rid
        : -1;
    let request: PopupRequest;
    try {
      request = parsePopupRequest(raw);
    } catch (error) {
      return { rid, outcome: { ok: false, error: errorInfo(error) } };
    }
    try {
      return { rid: request.rid, outcome: await this.dispatch(request) };
    } catch (error) {
      return { rid: request.rid, outcome: { ok: false, error: errorInfo(error) } };
    }
  }

  private async dispatch(request: PopupRequest): Promise<Outcome<unknown>> {
    const { host } = this.deps;
    switch (request.op) {
      case 'hello':
        return host.call('hello', {}, parseHello);
      case 'destinations':
        return host.call('destinations', {}, parseDestinations);
      case 'extract':
        return { ok: true, result: await this.deps.extract(request.tabId) };
      case 'preview': {
        const outcome = await host.call('capture.preview', { capture: request.capture }, parsePreview);
        if (outcome.ok) await this.track(outcome.result);
        return outcome;
      }
      case 'commit': {
        const open = this.previews.get(request.preview_id);
        if (!open) throw new BoundaryError('not_found', 'This preview is no longer open; preview again.');
        const decisions = checkDecisions(request.decisions, open.candidates);
        if (request.pdf !== null && (request.pdf >= open.pdfs || request.pdf >= MAX_PDF_CANDIDATES)) {
          throw new BoundaryError('invalid_request', 'PDF choice is not one of the preview candidates');
        }
        const fields: Record<string, unknown> = { preview_id: request.preview_id, decisions };
        if (request.pdf !== null) fields.pdf = request.pdf;
        const outcome = await host.call('capture.commit', fields, parseCommit);
        // Committed previews are consumed by the daemon; a conflict means preview again.
        if (outcome.ok || outcome.error.code === 'conflict' || outcome.error.code === 'not_found') {
          this.previews.delete(request.preview_id);
        }
        return outcome;
      }
      case 'cancel': {
        this.previews.delete(request.preview_id);
        return host.call('capture.cancel', { preview_id: request.preview_id }, parseCancel);
      }
      case 'pdf_job':
        return host.call('capture.pdf_job', { job: request.job }, parseJob);
      case 'retry_pdf':
        return host.call('capture.retry_pdf', { job: request.job }, parseJob);
    }
  }

  /** Records a new preview and cancels the ones it supersedes. */
  private async track(preview: PreviewResult): Promise<void> {
    const superseded = [...this.previews.keys()].filter((id) => id !== preview.preview_id);
    this.previews.set(preview.preview_id, { candidates: preview.candidates.length, pdfs: preview.pdf_candidates.length });
    if (this.closed) superseded.push(preview.preview_id);
    await this.cancelAll(superseded);
  }

  private async cancelAll(ids: string[]): Promise<void> {
    for (const id of ids) {
      this.previews.delete(id);
      // Best effort: an expired or unknown preview is fine.
      await this.deps.host.call('capture.cancel', { preview_id: id }, parseCancel).catch(() => undefined);
    }
  }

  /** The popup closed: cancel previews that were never committed. */
  async close(): Promise<void> {
    this.closed = true;
    await this.cancelAll([...this.previews.keys()]);
  }

  openPreviews(): string[] {
    return [...this.previews.keys()];
  }
}
