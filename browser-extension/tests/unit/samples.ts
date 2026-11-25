// Host reply samples in the exact cm-protocol serde shapes (see docs/ADR/native-messaging.md).
export const PAPER = '01a20f9f-b03c-7504-afba-21e79adc9288';
export const JOB = '01a20f9f-b03c-7504-afba-21e79adc9299';
export const COLLECTION = '01a20f9f-0000-7000-8000-000000000001';
export const CHILD = '01a20f9f-0000-7000-8000-000000000002';
export const TAG = '01a20f9f-0000-7000-8000-0000000000aa';

/** The daemon's constant redaction note (#27 `REDACTED_PREVIEW_WARNING`). */
export const REDACTED_WARNING =
  'Existing-library matches are hidden for this client; rows that exactly duplicate an existing paper are skipped on commit.';

export function previewResult(
  over: { matches?: unknown[]; warnings?: string[]; provisional?: boolean; pdfs?: unknown[]; id?: string; title?: string; hiddenDuplicates?: number; rows?: number } = {},
) {
  const draft = {
    title: over.title ?? 'Capturing Papers',
    external_ids: [{ namespace: 'doi', value: '10.9999/x' }],
    bibliography: {
      year: 2026,
      doi: '10.9999/x',
      journal: 'Journal of Fixtures',
      authors: [{ given_name: 'Ada', family_name: 'Lovelace', literal_name: null, orcid: null, role: 'author' }],
    },
    source: { adapter: 'browser-capture', locator: null, row: null, sha256: null },
    diagnostics: [],
  };
  const candidate = { draft, existing_match: null, matches: over.matches ?? [], batch_matches: [], diagnostics: [] };
  return {
    preview: {
      id: over.id ?? 'preview-1',
      candidates: Array.from({ length: over.rows ?? 1 }, () => candidate),
      warnings: over.warnings ?? [],
    },
    resolution: {
      page_url: 'https://journal.example.org/a',
      draft,
      identifiers: [],
      fields: [],
      tiers_used: ['highwire'],
      provider_outcomes: [],
      provisional: over.provisional ?? false,
      pdf_candidates: over.pdfs ?? [{ url: 'https://cdn.example.org/a.pdf', origin: 'highwire' }],
    },
    ...(over.hiddenDuplicates === undefined ? {} : { hidden_duplicates: over.hiddenDuplicates }),
  };
}

export function job(state: string, attempts = 1, error: string | null = null) {
  return {
    id: JOB,
    kind: 'capture.pdf_fetch',
    idempotency_key: 'capture-pdf:x',
    state,
    attempts,
    max_attempts: 4,
    progress: {},
    error,
    result: null,
    detected_committed: false,
    cancel_requested: false,
    lease_generation: 1,
    next_attempt_at_ms: null,
    deadline_at_ms: null,
    created_at_ms: 1,
    updated_at_ms: 2,
  };
}

export function commitResult(pdfJob: unknown = job('queued', 0)) {
  return {
    outcome: { created: [PAPER], merged: [], skipped: 0, rows: [] },
    paper: PAPER,
    pdf_job: pdfJob,
    pdf_error: null,
  };
}

/** The daemon skipped the (hidden duplicate) row: nothing created, no paper, no PDF job. */
export function skippedResult() {
  return {
    outcome: { created: [], merged: [], skipped: 1, rows: [] },
    paper: null,
    pdf_job: null,
    pdf_error: null,
  };
}

export const DESTINATIONS = {
  collections: [
    { id: CHILD, parent: COLLECTION, name: 'Child', sort_order: 0 },
    { id: COLLECTION, parent: null, name: 'Reading', sort_order: 0 },
  ],
  tags: [{ id: TAG, name: 'to-read', key: 'to-read', color: null }],
};

export const HELLO = { host_version: '0.1.0', protocol: 1, browser: 'chrome', paired: true, types: [], limits: {} };

/** A host envelope echoing `request`. */
export function ok(request: { id: string; type: string }, result: unknown) {
  return { v: 1, id: request.id, ok: true, type: request.type, result };
}
export function err(request: { id: string }, code: string, retryable = false) {
  return { v: 1, id: request.id, ok: false, error: { code, message: `${code} message`, retryable } };
}
