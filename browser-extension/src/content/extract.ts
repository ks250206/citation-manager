// Page snapshot extractor. Injected with `scripting.executeScript` into the top frame of the
// tab the user explicitly chose to save, and only then (activeTab; no host permissions).
//
// Everything read here is page data: it is copied as strings, never evaluated, and never
// used to decide what the extension may do. No page script is called; the snapshot HTML is
// built in an inert document (no browsing context: no script execution, no resource loads).
// All values are bounded to the #22 CaptureRequest limits on UTF-8 boundaries.
import {
  CORRECTION_RESERVE_BYTES,
  JSON_LD_BUDGET_BYTES,
  MAX_CAPTURE_BYTES,
  MAX_HTML_BYTES,
  MAX_JSON_LD_BLOCKS,
  MAX_JSON_LD_BYTES,
  MAX_META_CONTENT_BYTES,
  MAX_META_NAME_BYTES,
  MAX_META_TAGS,
  MAX_PAGE_URL_BYTES,
  MAX_TITLE_BYTES,
  META_BUDGET_BYTES,
} from '../shared/limits';
import { truncateUtf8, utf8Length, wellFormed } from '../shared/utf8';
import type { MetaTag, PageSnapshot } from '../shared/types';

/** Elements removed from the HTML snapshot: executable/irrelevant content, and <meta>
 * (sent as structured `meta`, so the daemon does not read every tag twice). */
const STRIPPED = 'script, style, noscript, template, meta, svg, math, canvas, iframe, frame, object, embed, video, audio';

export interface Budgets {
  html: number;
  meta: number;
  jsonLd: number;
}

export const DEFAULT_BUDGETS: Budgets = {
  html: MAX_HTML_BYTES,
  meta: META_BUDGET_BYTES,
  jsonLd: JSON_LD_BUDGET_BYTES,
};

function readMeta(doc: Document, budget: number): { meta: MetaTag[]; dropped: number; bytes: number } {
  const meta: MetaTag[] = [];
  let dropped = 0;
  let bytes = 0;
  for (const element of Array.from(doc.querySelectorAll('meta'))) {
    const rawName = element.getAttribute('name') ?? element.getAttribute('property');
    const rawContent = element.getAttribute('content');
    if (rawName === null || rawContent === null) continue;
    const name = wellFormed(rawName.trim());
    const content = wellFormed(rawContent);
    if (name === '') continue;
    const size = utf8Length(name) + utf8Length(content);
    if (
      utf8Length(name) > MAX_META_NAME_BYTES ||
      utf8Length(content) > MAX_META_CONTENT_BYTES ||
      meta.length >= MAX_META_TAGS ||
      bytes + size > budget
    ) {
      // Values are never cut mid-way: a shortened DOI or title would be wrong data.
      dropped++;
      continue;
    }
    bytes += size;
    meta.push({ name, content });
  }
  return { meta, dropped, bytes };
}

function readJsonLd(doc: Document, budget: number): { blocks: string[]; dropped: number; bytes: number } {
  const blocks: string[] = [];
  let dropped = 0;
  let bytes = 0;
  for (const script of Array.from(doc.querySelectorAll('script[type]'))) {
    const type = (script.getAttribute('type') ?? '').trim().toLowerCase();
    if (type !== 'application/ld+json') continue;
    // textContent of a data block: the browser never ran it (non-JavaScript type).
    const text = wellFormed((script.textContent ?? '').trim());
    if (text === '') continue;
    const size = utf8Length(text);
    if (size > MAX_JSON_LD_BYTES || blocks.length >= MAX_JSON_LD_BLOCKS || bytes + size > budget) {
      dropped++;
      continue;
    }
    bytes += size;
    blocks.push(text);
  }
  return { blocks, dropped, bytes };
}

/** A copy of the page markup in a document without a browsing context. */
function inertCopy(doc: Document): Element | null {
  const root = doc.documentElement;
  if (!root) return null;
  let copy: Element;
  try {
    const inert = doc.implementation.createHTMLDocument('');
    copy = inert.importNode(root, true);
  } catch {
    // Fallback: DOMParser documents are inert as well.
    const parsed = new DOMParser().parseFromString(root.outerHTML, 'text/html');
    if (!parsed.documentElement) return null;
    copy = parsed.documentElement;
  }
  for (const element of Array.from(copy.querySelectorAll(STRIPPED))) element.remove();
  return copy;
}

function isHttp(url: string): boolean {
  return /^https?:\/\//i.test(url);
}

export function extractSnapshot(doc: Document, pageUrl: string, budgets: Budgets = DEFAULT_BUDGETS): PageSnapshot {
  const page_url = wellFormed(pageUrl);
  const titleText = wellFormed((doc.title ?? '').trim());
  const title = titleText === '' ? null : truncateUtf8(titleText, MAX_TITLE_BYTES).text;
  const { meta, dropped: dropped_meta, bytes: metaBytes } = readMeta(doc, budgets.meta);
  const { blocks: json_ld, dropped: dropped_json_ld, bytes: jsonLdBytes } = readJsonLd(doc, budgets.jsonLd);
  const content_type = typeof doc.contentType === 'string' ? doc.contentType : 'text/html';
  // A PDF opened in the tab is the user's explicit PDF choice; otherwise the daemon finds
  // candidates in the page metadata itself (citation_pdf_url, JSON-LD, <link>).
  const pdf_url =
    content_type === 'application/pdf' && isHttp(page_url) && utf8Length(page_url) <= MAX_PAGE_URL_BYTES
      ? page_url
      : null;

  const used = (title === null ? 0 : utf8Length(title)) + metaBytes + jsonLdBytes;
  const htmlBudget = Math.max(0, Math.min(budgets.html, MAX_CAPTURE_BYTES - used - CORRECTION_RESERVE_BYTES));
  let html: string | null = null;
  let html_truncated = false;
  if (content_type !== 'application/pdf') {
    const copy = inertCopy(doc);
    if (copy) {
      const cut = truncateUtf8(copy.outerHTML, htmlBudget);
      html = cut.text === '' ? null : cut.text;
      html_truncated = cut.truncated;
    }
  }
  return { page_url, title, html, meta, json_ld, pdf_url, content_type, html_truncated, dropped_meta, dropped_json_ld };
}
