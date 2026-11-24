// Boundary checks: outgoing capture requests against the #22 limits, and host replies
// against the #23 v1 shapes. Replies are untrusted input too (a wrong or replaced host
// binary, a future version): anything that does not match becomes a `bad_reply` error and
// is never rendered as-is.
import {
  MAX_CAPTURE_BYTES,
  MAX_DECISIONS,
  MAX_HOST_REQUEST_BYTES,
  MAX_HTML_BYTES,
  MAX_JSON_LD_BLOCKS,
  MAX_JSON_LD_BYTES,
  MAX_META_CONTENT_BYTES,
  MAX_META_NAME_BYTES,
  MAX_META_TAGS,
  MAX_PAGE_URL_BYTES,
  MAX_PDF_CANDIDATES,
  MAX_TITLE_BYTES,
  PROTOCOL_VERSION,
} from './limits';
import { utf8Length } from './utf8';
import type {
  CaptureRequest,
  CommitResult,
  Contributor,
  Decision,
  DestinationsResult,
  Diagnostic,
  Draft,
  DuplicateMatch,
  ErrorInfo,
  HelloResult,
  JobState,
  JobStatus,
  MetaTag,
  Outcome,
  PdfCandidate,
  PreviewCandidate,
  PreviewResult,
} from './types';

export class BoundaryError extends Error {
  constructor(
    readonly code: string,
    message: string,
  ) {
    super(message);
  }
  info(): ErrorInfo {
    return { code: this.code, message: this.message, retryable: false };
  }
}

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === 'object' && v !== null && !Array.isArray(v);

export function isHttpUrl(value: string): boolean {
  try {
    const url = new URL(value);
    return url.protocol === 'http:' || url.protocol === 'https:';
  } catch {
    return false;
  }
}

// ---- outgoing ----------------------------------------------------------------------

function tooLarge(message: string): never {
  throw new BoundaryError('too_large', message);
}
function invalid(message: string): never {
  throw new BoundaryError('invalid_request', message);
}

/** Mirrors `CaptureRequest::validate` (cm_core::capture) plus shape checks. */
export function checkCaptureRequest(value: unknown): CaptureRequest {
  if (!isObject(value)) invalid('capture must be an object');
  const { page_url, title, html, meta, json_ld, pdf_url, destination, offline } = value;
  if (typeof page_url !== 'string' || page_url.trim() === '') invalid('page URL is required');
  if (utf8Length(page_url) > MAX_PAGE_URL_BYTES) tooLarge('page URL exceeds 4 KiB');
  if (!isHttpUrl(page_url)) invalid('only http(s) pages can be saved');
  if (title !== undefined && typeof title !== 'string') invalid('title must be a string');
  if (html !== undefined && typeof html !== 'string') invalid('html must be a string');
  if (pdf_url !== undefined && (typeof pdf_url !== 'string' || !isHttpUrl(pdf_url))) {
    invalid('pdf_url must be an http(s) URL');
  }
  if (typeof pdf_url === 'string' && utf8Length(pdf_url) > MAX_PAGE_URL_BYTES) tooLarge('PDF URL exceeds 4 KiB');
  if (typeof offline !== 'boolean') invalid('offline must be a boolean');
  if (!Array.isArray(meta) || !Array.isArray(json_ld)) invalid('meta and json_ld must be arrays');
  if (meta.length > MAX_META_TAGS) tooLarge('more than 1024 meta tags');
  if (json_ld.length > MAX_JSON_LD_BLOCKS) tooLarge('more than 32 JSON-LD blocks');
  const titleBytes = typeof title === 'string' ? utf8Length(title) : 0;
  if (titleBytes > MAX_TITLE_BYTES) tooLarge('title exceeds 16 KiB');
  const htmlBytes = typeof html === 'string' ? utf8Length(html) : 0;
  if (htmlBytes > MAX_HTML_BYTES) tooLarge('HTML snapshot exceeds 2 MiB');
  let total = titleBytes + htmlBytes;
  const cleanMeta: MetaTag[] = [];
  for (const tag of meta) {
    if (!isObject(tag) || typeof tag.name !== 'string' || typeof tag.content !== 'string') {
      invalid('meta tags must be {name, content} strings');
    }
    const n = utf8Length(tag.name);
    const c = utf8Length(tag.content);
    if (n > MAX_META_NAME_BYTES || c > MAX_META_CONTENT_BYTES) tooLarge('meta tag exceeds size limits');
    total += n + c;
    cleanMeta.push({ name: tag.name, content: tag.content });
  }
  for (const block of json_ld) {
    if (typeof block !== 'string') invalid('JSON-LD blocks must be strings');
    const b = utf8Length(block);
    if (b > MAX_JSON_LD_BYTES) tooLarge('JSON-LD block exceeds 512 KiB');
    total += b;
  }
  if (total > MAX_CAPTURE_BYTES) tooLarge('capture exceeds 4 MiB');
  if (!isObject(destination)) invalid('destination must be an object');
  const { collection, tags } = destination;
  if (collection !== null && collection !== undefined && !isUuid(collection)) invalid('collection must be an id');
  if (!Array.isArray(tags) || tags.length > 256 || !tags.every(isUuid)) invalid('tags must be ids');
  const request: CaptureRequest = {
    page_url,
    meta: cleanMeta,
    json_ld: json_ld as string[],
    destination: { collection: typeof collection === 'string' ? collection : null, tags: tags as string[] },
    offline,
  };
  if (typeof title === 'string') request.title = title;
  if (typeof html === 'string') request.html = html;
  if (typeof pdf_url === 'string') request.pdf_url = pdf_url;
  return request;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
export function isUuid(v: unknown): v is string {
  return typeof v === 'string' && UUID.test(v);
}

export function checkDecisions(value: unknown, candidates: number): Decision[] {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_DECISIONS) {
    invalid('decisions must have 1..=16 entries');
  }
  return value.map((d) => {
    if (
      !isObject(d) ||
      !Number.isInteger(d.candidate_index) ||
      (d.candidate_index as number) < 0 ||
      (d.candidate_index as number) >= candidates ||
      (d.action !== 'Create' && d.action !== 'Skip')
    ) {
      invalid('each decision is {candidate_index, action: Create|Skip}');
    }
    return { candidate_index: d.candidate_index as number, action: d.action };
  });
}

/** Serialized size of a host request, checked against the host's 8 MiB frame. */
export function checkFrameSize(message: unknown): void {
  if (utf8Length(JSON.stringify(message)) > MAX_HOST_REQUEST_BYTES) tooLarge('request exceeds the 8 MiB host frame');
}

// ---- incoming ----------------------------------------------------------------------

function bad(path: string): never {
  throw new BoundaryError('bad_reply', `unexpected reply from the native host (${path})`);
}
function str(v: unknown, path: string, max = 64 * 1024): string {
  if (typeof v !== 'string' || v.length > max) bad(path);
  return v;
}
function optStr(v: unknown, path: string, max = 64 * 1024): string | null {
  return v === null || v === undefined ? null : str(v, path, max);
}
function int(v: unknown, path: string): number {
  if (typeof v !== 'number' || !Number.isInteger(v)) bad(path);
  return v;
}
function optInt(v: unknown, path: string): number | null {
  return v === null || v === undefined ? null : int(v, path);
}
function bool(v: unknown, path: string): boolean {
  if (typeof v !== 'boolean') bad(path);
  return v;
}
function arr(v: unknown, path: string, max: number): unknown[] {
  if (v === undefined) return [];
  if (!Array.isArray(v) || v.length > max) bad(path);
  return v;
}
function obj(v: unknown, path: string): Record<string, unknown> {
  if (!isObject(v)) bad(path);
  return v;
}

function diagnostics(v: unknown, path: string): Diagnostic[] {
  return arr(v, path, 256).map((d, i) => {
    const o = obj(d, `${path}[${i}]`);
    return {
      field: str(o.field, `${path}[${i}].field`, 256),
      message: str(o.message, `${path}[${i}].message`, 4096),
      error: bool(o.error, `${path}[${i}].error`),
    };
  });
}

function contributors(v: unknown, path: string): Contributor[] {
  return arr(v, path, 4096).map((c, i) => {
    const o = obj(c, `${path}[${i}]`);
    return {
      given_name: optStr(o.given_name, `${path}[${i}].given_name`, 4096),
      family_name: optStr(o.family_name, `${path}[${i}].family_name`, 4096),
      literal_name: optStr(o.literal_name, `${path}[${i}].literal_name`, 4096),
    };
  });
}

/** `PaperDraft` (bibliography nested) or `Paper` (bibliography flattened). */
function draft(v: unknown, path: string, flattened: boolean): Draft {
  const o = obj(v, path);
  const b = flattened ? o : obj(o.bibliography ?? {}, `${path}.bibliography`);
  return {
    title: str(o.title, `${path}.title`),
    external_ids: arr(o.external_ids, `${path}.external_ids`, 64).map((e, i) => {
      const x = obj(e, `${path}.external_ids[${i}]`);
      return { namespace: str(x.namespace, `${path}.external_ids[${i}]`, 64), value: str(x.value, `${path}.external_ids[${i}]`, 4096) };
    }),
    doi: optStr(b.doi, `${path}.doi`, 4096),
    year: optInt(b.year, `${path}.year`),
    journal: optStr(b.journal, `${path}.journal`, 4096),
    url: optStr(b.url, `${path}.url`, 8192),
    authors: contributors(b.authors, `${path}.authors`),
    diagnostics: flattened ? [] : diagnostics(o.diagnostics, `${path}.diagnostics`),
  };
}

export function parseHello(v: unknown): HelloResult {
  const o = obj(v, 'result');
  return {
    host_version: str(o.host_version, 'host_version', 64),
    protocol: int(o.protocol, 'protocol'),
    browser: str(o.browser, 'browser', 64),
    paired: bool(o.paired, 'paired'),
    types: arr(o.types, 'types', 64).map((t) => str(t, 'types[]', 64)),
  };
}

export function parseDestinations(v: unknown): DestinationsResult {
  const o = obj(v, 'result');
  return {
    collections: arr(o.collections, 'collections', 10000).map((c, i) => {
      const x = obj(c, `collections[${i}]`);
      if (!isUuid(x.id) || (x.parent !== null && x.parent !== undefined && !isUuid(x.parent))) bad(`collections[${i}].id`);
      return {
        id: x.id,
        parent: (x.parent as string | null | undefined) ?? null,
        name: str(x.name, `collections[${i}].name`, 4096),
        sort_order: int(x.sort_order ?? 0, `collections[${i}].sort_order`),
      };
    }),
    tags: arr(o.tags, 'tags', 10000).map((t, i) => {
      const x = obj(t, `tags[${i}]`);
      if (!isUuid(x.id)) bad(`tags[${i}].id`);
      return { id: x.id, name: str(x.name, `tags[${i}].name`, 4096), color: optStr(x.color, `tags[${i}].color`, 64) };
    }),
  };
}

export function parsePreview(v: unknown): PreviewResult {
  const o = obj(v, 'result');
  const preview = obj(o.preview, 'preview');
  const resolution = obj(o.resolution, 'resolution');
  const candidates: PreviewCandidate[] = arr(preview.candidates, 'preview.candidates', 16).map((c, i) => {
    const x = obj(c, `candidates[${i}]`);
    const matches: DuplicateMatch[] = arr(x.matches, `candidates[${i}].matches`, 64).map((m, j) => {
      const mm = obj(m, `matches[${j}]`);
      const existing = draft(mm.existing, `matches[${j}].existing`, true);
      return { kind: str(mm.kind, `matches[${j}].kind`, 64), title: existing.title, year: existing.year, doi: existing.doi };
    });
    return {
      draft: draft(x.draft, `candidates[${i}].draft`, false),
      matches,
      exact_duplicate: matches.some((m) => m.kind === 'Doi' || m.kind === 'ExternalId'),
      diagnostics: diagnostics(x.diagnostics, `candidates[${i}].diagnostics`),
    };
  });
  if (candidates.length === 0) bad('preview.candidates');
  const pdfs: PdfCandidate[] = arr(resolution.pdf_candidates, 'pdf_candidates', MAX_PDF_CANDIDATES).map((p, i) => {
    const x = obj(p, `pdf_candidates[${i}]`);
    const url = str(x.url, `pdf_candidates[${i}].url`, 8192);
    if (!isHttpUrl(url)) bad(`pdf_candidates[${i}].url`);
    return { url, origin: str(x.origin, `pdf_candidates[${i}].origin`, 32) };
  });
  const warnings = arr(preview.warnings, 'preview.warnings', 64).map((w) => str(w, 'warnings[]', 4096));
  // The daemon reports redacted duplicate matches only as a warning (#25 redact_matches).
  const hidden = warnings.map((w) => /^(\d+) possible existing-library match(?:es)? hidden/.exec(w)).find(Boolean);
  return {
    preview_id: str(preview.id, 'preview.id', 256),
    candidates,
    warnings,
    hidden_matches: hidden ? Number(hidden[1]) : 0,
    provisional: bool(resolution.provisional ?? false, 'resolution.provisional'),
    pdf_candidates: pdfs,
    tiers_used: arr(resolution.tiers_used, 'tiers_used', 16).map((t) => str(t, 'tiers_used[]', 32)),
  };
}

const JOB_STATES: readonly JobState[] = ['queued', 'running', 'retry_wait', 'succeeded', 'failed', 'cancelled'];

export function parseJob(v: unknown): JobStatus {
  const o = obj(v, 'job');
  if (!isUuid(o.id)) bad('job.id');
  const state = o.state;
  if (typeof state !== 'string' || !(JOB_STATES as readonly string[]).includes(state)) bad('job.state');
  return {
    id: o.id,
    state: state as JobState,
    attempts: int(o.attempts, 'job.attempts'),
    max_attempts: int(o.max_attempts ?? 0, 'job.max_attempts'),
    error: optStr(o.error, 'job.error', 4096),
    detected_committed: bool(o.detected_committed ?? false, 'job.detected_committed'),
  };
}

export function parseCommit(v: unknown): CommitResult {
  const o = obj(v, 'result');
  const outcome = obj(o.outcome, 'outcome');
  const paper = o.paper === null || o.paper === undefined ? null : o.paper;
  if (paper !== null && !isUuid(paper)) bad('paper');
  return {
    paper,
    created: arr(outcome.created, 'outcome.created', 16).length,
    skipped: int(outcome.skipped ?? 0, 'outcome.skipped'),
    pdf_job: o.pdf_job === null || o.pdf_job === undefined ? null : parseJob(o.pdf_job),
    pdf_error: optStr(o.pdf_error, 'pdf_error', 4096),
  };
}

export function parseCancel(v: unknown): { cancelled: boolean } {
  return { cancelled: bool(obj(v, 'result').cancelled, 'cancelled') };
}

/**
 * Validates one host reply envelope for request `id` of `type`, then the result with `parse`.
 * A host error keeps its code and retryable flag; its message is bounded.
 */
export function checkReply<T>(raw: unknown, id: string, type: string, parse: (v: unknown) => T): Outcome<T> {
  if (!isObject(raw)) bad('envelope');
  if (raw.v !== PROTOCOL_VERSION) bad('v');
  if (raw.ok === false) {
    // The host echoes null when it could not read the id (bad frame); otherwise ours.
    if (raw.id !== id && raw.id !== null) bad('id');
    const e = obj(raw.error, 'error');
    return {
      ok: false,
      error: {
        code: str(e.code, 'error.code', 64),
        message: str(e.message, 'error.message', 2048),
        retryable: bool(e.retryable, 'error.retryable'),
      },
    };
  }
  if (raw.ok !== true || raw.id !== id) bad('id');
  if (raw.type !== type) bad('type');
  return { ok: true, result: parse(raw.result) };
}
