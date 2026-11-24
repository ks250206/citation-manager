// Wire shapes of the #23 host contract (v1) and the #22/#33 result objects the extension
// reads. Only the fields the extension uses are typed; unknown fields are ignored, as the
// contract allows additive fields within a version.

export interface MetaTag {
  name: string;
  content: string;
}

/** What the injected content script returns for the page the user chose to save. */
export interface PageSnapshot {
  page_url: string;
  title: string | null;
  /** Inert HTML snapshot with <script>/<style>/<meta> elements removed (sent separately). */
  html: string | null;
  meta: MetaTag[];
  json_ld: string[];
  /** Set only when the saved document itself is a PDF (the user's explicit choice). */
  pdf_url: string | null;
  content_type: string;
  html_truncated: boolean;
  /** Meta tags / JSON-LD blocks left out because of the contract limits. */
  dropped_meta: number;
  dropped_json_ld: number;
}

export interface Destination {
  collection: string | null;
  tags: string[];
}

/** #22 `CaptureRequest`. */
export interface CaptureRequest {
  page_url: string;
  title?: string;
  html?: string;
  meta: MetaTag[];
  json_ld: string[];
  pdf_url?: string;
  destination: Destination;
  offline: boolean;
}

/** User corrections of a provisional or wrong preview; empty strings mean "unchanged". */
export interface Corrections {
  title?: string;
  doi?: string;
  /** One author per entry, "Family, Given" or a literal name. */
  authors?: string[];
  year?: string;
  journal?: string;
}

export type Action = 'Create' | 'Skip';

export interface Decision {
  candidate_index: number;
  action: Action;
}

// ---- results -------------------------------------------------------------------------

export interface HelloResult {
  host_version: string;
  protocol: number;
  browser: string;
  paired: boolean;
  types: string[];
}

export interface Collection {
  id: string;
  parent: string | null;
  name: string;
  sort_order: number;
}

export interface Tag {
  id: string;
  name: string;
  color: string | null;
}

export interface DestinationsResult {
  collections: Collection[];
  tags: Tag[];
}

export interface Contributor {
  given_name: string | null;
  family_name: string | null;
  literal_name: string | null;
}

export interface ExternalId {
  namespace: string;
  value: string;
}

export interface Diagnostic {
  field: string;
  message: string;
  error: boolean;
}

export interface Draft {
  title: string;
  external_ids: ExternalId[];
  doi: string | null;
  year: number | null;
  journal: string | null;
  url: string | null;
  authors: Contributor[];
  diagnostics: Diagnostic[];
}

export interface DuplicateMatch {
  kind: string;
  title: string;
  year: number | null;
  doi: string | null;
}

export interface PreviewCandidate {
  draft: Draft;
  matches: DuplicateMatch[];
  /** An exact (DOI / external id) persisted match: the host grant cannot create it. */
  exact_duplicate: boolean;
  diagnostics: Diagnostic[];
}

export interface PdfCandidate {
  url: string;
  origin: string;
}

export interface PreviewResult {
  preview_id: string;
  candidates: PreviewCandidate[];
  warnings: string[];
  /** Existing-library matches the daemon hid from this grant (no LibraryRead). */
  hidden_matches: number;
  provisional: boolean;
  pdf_candidates: PdfCandidate[];
  tiers_used: string[];
}

export type JobState = 'queued' | 'running' | 'retry_wait' | 'succeeded' | 'failed' | 'cancelled';

export interface JobStatus {
  id: string;
  state: JobState;
  attempts: number;
  max_attempts: number;
  error: string | null;
  detected_committed: boolean;
}

export interface CommitResult {
  paper: string | null;
  created: number;
  skipped: number;
  pdf_job: JobStatus | null;
  pdf_error: string | null;
}

/** Host error object, or a client-side error with an extension-local code. */
export interface ErrorInfo {
  code: string;
  message: string;
  retryable: boolean;
}

export type Outcome<T> = { ok: true; result: T } | { ok: false; error: ErrorInfo };
