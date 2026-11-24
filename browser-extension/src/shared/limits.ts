// Limits of the #22 CaptureRequest and the #23 Native Messaging contract
// (docs/ADR/browser-capture.md, docs/ADR/native-messaging.md). The host and the daemon
// enforce them again; the extension checks first so it never sends a request that would be
// refused for its size, and truncates the page snapshot so it fits.
export const KiB = 1024;
export const MiB = 1024 * KiB;

export const HOST_NAME = 'dev.citation_manager.native_host';
export const PROTOCOL_VERSION = 1;

export const MAX_PAGE_URL_BYTES = 4 * KiB;
export const MAX_TITLE_BYTES = 16 * KiB;
export const MAX_HTML_BYTES = 2 * MiB;
export const MAX_META_TAGS = 1024;
export const MAX_META_NAME_BYTES = 256;
export const MAX_META_CONTENT_BYTES = 64 * KiB;
export const MAX_JSON_LD_BLOCKS = 32;
export const MAX_JSON_LD_BYTES = 512 * KiB;
/** html + title + meta + JSON-LD together. */
export const MAX_CAPTURE_BYTES = 4 * MiB;
/** Browser -> host frame limit enforced by papers-native-host. */
export const MAX_HOST_REQUEST_BYTES = 8 * MiB;
/** Host -> browser frame limit (papers-native-host never sends more). */
export const MAX_HOST_REPLY_BYTES = 1 * MiB;
export const MAX_DECISIONS = 16;
export const MAX_PDF_CANDIDATES = 16;

// Extension-side budgets inside MAX_CAPTURE_BYTES. Meta tags and JSON-LD are kept whole
// (never cut mid-value); the HTML snapshot gets what is left.
export const META_BUDGET_BYTES = 512 * KiB;
export const JSON_LD_BUDGET_BYTES = 1 * MiB;
/** Room for manual corrections, which travel as extra meta tags. */
export const CORRECTION_RESERVE_BYTES = 64 * KiB;

/** Request ids echoed by the host: 1..=64 of [A-Za-z0-9._:-]. */
export const REQUEST_ID_PATTERN = /^[A-Za-z0-9._:-]{1,64}$/;
