// Builds the #22 CaptureRequest from a page snapshot, the chosen destination and optional
// manual corrections.
//
// The #22/#23 contract has no "edited draft" field: commit only takes Create/Skip decisions
// over the daemon's own preview. Corrections are therefore sent as a new preview whose page
// metadata carries the user's values as Highwire `citation_*` meta tags placed ahead of the
// page's own tags (the highest page tier; the snapshot HTML carries no <meta> elements, so
// nothing in it competes). The daemon still validates and normalizes every value, and lower
// tiers (JSON-LD, Dublin Core) only fill fields that remain empty.
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

const clean = (value: string): string => truncateUtf8(wellFormed(value).trim(), MAX_META_CONTENT_BYTES).text;

/** Normalizes corrections: trims, drops empty values, bounds authors. */
export function normalizeCorrections(input: Corrections): Corrections {
  const out: Corrections = {};
  for (const key of ['title', 'doi', 'year', 'journal'] as const) {
    const value = input[key];
    if (typeof value === 'string' && value.trim() !== '') out[key] = clean(value);
  }
  if (input.authors) {
    const authors = input.authors.map(clean).filter((a) => a !== '').slice(0, 512);
    if (authors.length > 0) out.authors = authors;
  }
  if (out.year !== undefined && !/^\d{4}$/.test(out.year)) {
    throw new RangeError('year must be four digits');
  }
  return out;
}

export function hasCorrections(c: Corrections): boolean {
  return Object.keys(c).length > 0;
}

export function correctionMeta(c: Corrections): MetaTag[] {
  const tags: MetaTag[] = [];
  if (c.title) tags.push({ name: 'citation_title', content: c.title });
  for (const author of c.authors ?? []) tags.push({ name: 'citation_author', content: author });
  if (c.year) tags.push({ name: 'citation_publication_date', content: c.year });
  if (c.journal) tags.push({ name: 'citation_journal_title', content: c.journal });
  if (c.doi) tags.push({ name: 'citation_doi', content: c.doi });
  return tags;
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
    meta: [...correctionMeta(corrections), ...pageMeta].slice(0, MAX_META_TAGS),
    json_ld: snapshot.json_ld,
    destination: { collection: destination.collection, tags: [...destination.tags] },
    offline,
  };
  if (snapshot.title !== null) request.title = snapshot.title;
  if (snapshot.html !== null) request.html = snapshot.html;
  if (snapshot.pdf_url !== null) request.pdf_url = snapshot.pdf_url;
  return request;
}
