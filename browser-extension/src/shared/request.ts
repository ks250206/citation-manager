// Builds the #22 CaptureRequest from a page snapshot, the chosen destination and optional
// manual corrections.
//
// The #22/#23 contract has no "edited draft" field: commit only takes Create/Skip decisions
// over the daemon's own preview. Corrections are therefore sent as a new preview whose page
// metadata carries the user's values as Highwire `citation_*` meta tags placed ahead of the
// page's own tags (the highest page tier; the snapshot HTML carries no <meta> elements, so
// nothing in it competes). The daemon still validates and normalizes every value, and lower
// tiers (JSON-LD, Dublin Core) only fill fields that remain empty.
//
// Clearing: an empty DOI/authors/year/journal correction removes the page's `citation_*`
// tags for that field without adding a value. A lower tier (JSON-LD, Dublin Core) or the DOI
// lookup may still fill it; the popup says so. The title cannot be cleared (the daemon would
// fall back to the page title).
import { MAX_META_CONTENT_BYTES, MAX_META_TAGS } from './limits';
import { truncateUtf8, wellFormed } from './utf8';
import type { CaptureRequest, Corrections, Destination, MetaTag, PageSnapshot } from './types';

/** Meta names each correction replaces (lowercase, as the daemon compares them). */
const REPLACED: Record<keyof Corrections, string[]> = {
  title: ['citation_title'],
  doi: ['citation_doi'],
  authors: ['citation_author', 'citation_authors'],
  year: ['citation_publication_date', 'citation_date', 'citation_cover_date', 'citation_online_date', 'citation_year'],
  journal: [
    'citation_journal_title',
    'citation_conference_title',
    'citation_inbook_title',
    'citation_book_title',
  ],
};

/** Container tags in the daemon's order; the first present on the page decides the type. */
const CONTAINERS = REPLACED.journal;

/** Fields that may be cleared (an empty correction). */
export const CLEARABLE = ['doi', 'year', 'journal', 'authors'] as const;

const clean = (value: string): string => truncateUtf8(wellFormed(value).trim(), MAX_META_CONTENT_BYTES).text;

/**
 * Normalizes corrections: trims and bounds values. An empty DOI/year/journal (`''`) or an
 * empty author list (`[]`) means "clear"; an empty title is refused.
 */
export function normalizeCorrections(input: Corrections): Corrections {
  const out: Corrections = {};
  if (input.title !== undefined) {
    const title = clean(input.title);
    if (title === '') throw new RangeError('The title cannot be empty.');
    out.title = title;
  }
  for (const key of ['doi', 'year', 'journal'] as const) {
    const value = input[key];
    if (typeof value === 'string') out[key] = clean(value);
  }
  if (input.authors) out.authors = input.authors.map(clean).filter((a) => a !== '').slice(0, 512);
  if (out.year && !/^\d{4}$/.test(out.year)) {
    throw new RangeError('The year must be four digits (or empty to clear it).');
  }
  return out;
}

/** Corrected fields that the user cleared. */
export function clearedFields(c: Corrections): string[] {
  return CLEARABLE.filter((k) => (k === 'authors' ? c.authors?.length === 0 : c[k] === ''));
}

export function hasCorrections(c: Corrections): boolean {
  return Object.keys(c).length > 0;
}

/**
 * Meta tags carrying the corrections. A corrected container keeps the page's own container
 * tag (journal, conference, book...), so the daemon's paper type is not changed to
 * journal-article; without one on the page it is `citation_journal_title`.
 */
export function correctionMeta(c: Corrections, pageMeta: MetaTag[] = []): MetaTag[] {
  const tags: MetaTag[] = [];
  if (c.title) tags.push({ name: 'citation_title', content: c.title });
  for (const author of c.authors ?? []) tags.push({ name: 'citation_author', content: author });
  if (c.year) tags.push({ name: 'citation_publication_date', content: c.year });
  if (c.journal) {
    const names = new Set(pageMeta.map((m) => m.name.trim().toLowerCase()));
    const container = CONTAINERS.find((n) => names.has(n)) ?? 'citation_journal_title';
    tags.push({ name: container, content: c.journal });
  }
  if (c.doi) tags.push({ name: 'citation_doi', content: c.doi });
  return tags;
}

/** Canonical DOI text for comparison (prefixes and case removed). */
export function doiKey(doi: string): string {
  return doi
    .trim()
    .replace(/^(?:https?:\/\/(?:dx\.)?doi\.org\/|doi:\s*)/i, '')
    .toLowerCase();
}

export function buildCaptureRequest(
  snapshot: PageSnapshot,
  destination: Destination,
  offline: boolean,
  corrections: Corrections = {},
): CaptureRequest {
  const replaced = new Set<string>();
  for (const key of Object.keys(corrections) as (keyof Corrections)[]) {
    for (const name of REPLACED[key]) replaced.add(name);
  }
  const pageMeta = snapshot.meta.filter((m) => !replaced.has(m.name.trim().toLowerCase()));
  const request: CaptureRequest = {
    page_url: snapshot.page_url,
    // Corrections first; the page's own tags fill the rest of the 1024-tag limit.
    meta: [...correctionMeta(corrections, snapshot.meta), ...pageMeta].slice(0, MAX_META_TAGS),
    json_ld: snapshot.json_ld,
    destination: { collection: destination.collection, tags: [...destination.tags] },
    offline,
  };
  if (snapshot.title !== null) request.title = snapshot.title;
  if (snapshot.html !== null) request.html = snapshot.html;
  if (snapshot.pdf_url !== null) request.pdf_url = snapshot.pdf_url;
  return request;
}
