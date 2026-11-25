// Background trust boundaries (DESIGN §12.3): who may talk to the background, and what the
// injected content script may hand back.
import type { InjectionResult, MessageSender, Tab } from '../shared/api';
import { MAX_JSON_LD_BLOCKS, MAX_META_TAGS, MAX_PAGE_URL_BYTES } from '../shared/limits';
import type { MetaTag, PageSnapshot } from '../shared/types';
import { utf8Length } from '../shared/utf8';
import { BoundaryError, isHttpUrl } from '../shared/validate';

/**
 * Only this extension's own popup page may open the capture port: same extension id and the
 * exact popup document (as the toolbar popup, or opened in a tab with `?tab=`). Content
 * scripts never connect, so a port from any http(s) document, another extension or a frame
 * is refused.
 */
export function isPopupSender(sender: MessageSender | undefined, runtimeId: string, popupUrl: string): boolean {
  if (!sender || sender.id !== runtimeId) return false;
  if (sender.frameId !== undefined && sender.frameId !== 0) return false;
  const url = sender.url;
  if (typeof url !== 'string') return false;
  if (url !== popupUrl && !url.startsWith(`${popupUrl}?`) && !url.startsWith(`${popupUrl}#`)) return false;
  // `origin` is reported by Chrome; when present it must be the extension's own.
  // (Computed by hand: URL.origin is "null" for non-special schemes in some engines.)
  const origin = popupUrl.slice(0, popupUrl.indexOf('/', popupUrl.indexOf('://') + 3));
  if (sender.origin !== undefined && sender.origin !== 'null' && sender.origin !== origin) return false;
  return true;
}

function bad(message: string): never {
  throw new BoundaryError('bad_snapshot', message);
}

function withoutHash(url: string): string {
  const u = new URL(url);
  u.hash = '';
  return u.href;
}

/**
 * Validates the content script's return value for `tab`. It must come from the top frame,
 * the tab must be an http(s) page that this extension may read (activeTab exposes its URL),
 * and the snapshot must describe that same page. Sizes are checked again when the capture
 * request is built (`checkCaptureRequest`).
 */
export function checkSnapshot(results: InjectionResult[] | undefined, tab: Tab): PageSnapshot {
  const tabUrl = tab.url;
  if (typeof tabUrl !== 'string' || !isHttpUrl(tabUrl)) {
    throw new BoundaryError('unsupported_page', 'Only http(s) pages can be saved.');
  }
  const top = (results ?? []).find((r) => r.frameId === 0);
  if (!top) bad('no result from the page');
  const s = top.result;
  if (typeof s !== 'object' || s === null || Array.isArray(s)) bad('snapshot is not an object');
  const o = s as Record<string, unknown>;
  const optString = (v: unknown, name: string): string | null => {
    if (v === null) return null;
    if (typeof v !== 'string') bad(name);
    return v;
  };
  if (typeof o.page_url !== 'string' || utf8Length(o.page_url) > MAX_PAGE_URL_BYTES || !isHttpUrl(o.page_url)) bad('page_url');
  if (withoutHash(o.page_url) !== withoutHash(tabUrl)) bad('the page changed while it was read; try again');
  if (!Array.isArray(o.meta) || o.meta.length > MAX_META_TAGS) bad('meta');
  const meta: MetaTag[] = o.meta.map((m: unknown) => {
    if (typeof m !== 'object' || m === null) bad('meta');
    const { name, content } = m as Record<string, unknown>;
    if (typeof name !== 'string' || typeof content !== 'string') bad('meta');
    return { name, content };
  });
  if (!Array.isArray(o.json_ld) || o.json_ld.length > MAX_JSON_LD_BLOCKS || !o.json_ld.every((b) => typeof b === 'string')) {
    bad('json_ld');
  }
  const pdf = optString(o.pdf_url ?? null, 'pdf_url');
  if (pdf !== null && pdf !== o.page_url) bad('pdf_url');
  const count = (v: unknown): number => (typeof v === 'number' && Number.isInteger(v) && v >= 0 ? v : 0);
  return {
    page_url: o.page_url,
    title: optString(o.title ?? null, 'title'),
    html: optString(o.html ?? null, 'html'),
    meta,
    json_ld: o.json_ld as string[],
    pdf_url: pdf,
    content_type: typeof o.content_type === 'string' ? o.content_type.slice(0, 128) : 'text/html',
    html_truncated: o.html_truncated === true,
    dropped_meta: count(o.dropped_meta),
    dropped_json_ld: count(o.dropped_json_ld),
  };
}
