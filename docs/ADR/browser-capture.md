# #22: Browser/URL capture service, safe public PDF fetch and retry

Status: implemented and tested on Linux (daemon side). The native messaging host (#23), the
browser extension (#24), macOS packaging (#34) and MCP tools (#29) call this contract; they are
not part of this change. Evidence: [ISSUE_22](../validation/ISSUE_22.md).

## Flow

```text
extension/MCP ──CaptureRequest──▶ native host ──IPC Command::Capture──▶ papersd
  Preview:  validate bounds → inert extraction (cm_metadata::html) → optional #14 DOI
            complement (fixed Crossref/DataCite endpoints) → common #33 preview (no write)
  Commit:   own capture preview + reviewed decisions → common #33 commit (paper, collection,
            tags in one transaction) → optional `capture.pdf_fetch` durable job
  PdfJob / RetryPdf: status / explicit retry of the grant's own PDF job
```

`cm_core::capture` owns the transport-neutral DTOs (`CaptureRequest`, `PageMetadata`,
`CaptureResolution`) and the pure resolution policy; `cm_metadata::{html, capture,
fetch_policy}` own HTML scanning, composition and HTTP; `cm_protocol::capture` adds the IPC
commands; `cm_daemon` authorizes, binds previews to grants and runs the job. No new crate
edge. Captures reuse `ImportCoordinator` unchanged: normalization, duplicate detection
(DOI → arXiv/PMID → title/author/year → similar title), field-diff merge, citation keys,
destination validation, stale-revision checks and cancel are the #33 ones.

## Resolution order (DESIGN §8.2)

1. **Identifiers**: DOI / arXiv / PMID from `citation_*`, JSON-LD, Dublin Core/PRISM, the
   page URL (doi.org, arxiv.org, PubMed), and — only when there is no other DOI — a page's
   *single* distinct DOI link (pages with several DOI links, e.g. reference lists, are not
   guessed; a diagnostic says so). Each is canonicalized by #33 (`normalize_doi`,
   `external_id`), arXiv/PMID additionally by shape. Conflicts keep the higher tier and add a
   diagnostic.
2. **Page metadata tiers**: Highwire `citation_*` → JSON-LD (`ScholarlyArticle` and close
   schema.org types, `@graph`, `isPartOf` chains) → Dublin Core/PRISM. Lower tiers only fill
   fields that are absent above; differing values are recorded in
   `extra_json.capture.conflicts` (bounded) and never applied. A lower-tier date that disagrees
   with the selected year is dropped (same date/year group rule as #14).
3. **Provider complement** (#14): only when a DOI was found (or, for an arXiv-only page,
   arXiv's DataCite DOI `10.48550/arXiv.<id>` — added to the record only if a provider
   confirms it) **and** title, authors, year or container is still missing, and the request
   is not `offline`. Provider values fill absent fields only; outcomes are returned, failures
   become non-error diagnostics and page metadata is kept. There is no PMID provider.
4. **Provisional**: no title → `og:title`/`document.title`/`<title>`, else the page URL text,
   with `provisional = true` and a review diagnostic; a public page URL fills the URL field.

Invalid page values (bad identifiers, impossible dates, oversized text, control characters)
are dropped with diagnostics instead of making the row unreviewable. If the draft exceeds the
common 64 KiB budget, abstract, provider provenance, conflict log, contributors and finally all
extra JSON are shed in that order with a diagnostic; nothing is truncated mid-value.

## Untrusted input

- `CaptureRequest` bounds: page URL ≤ 4 KiB, title ≤ 16 KiB, HTML ≤ 2 MiB, ≤ 1024 meta tags
  (name ≤ 256 B, content ≤ 64 KiB), ≤ 32 JSON-LD blocks of ≤ 512 KiB, 4 MiB in total; checked
  before any work (the IPC request frame is 8 MiB).
- The HTML scanner is inert and work-bounded: no DOM, no script execution, comments/
  `<script>`/`<style>`/`<template>` bodies skipped (only `application/ld+json` is read), basic
  and numeric entities only (decoded on char boundaries), ≤ 100k tags, bounded collections.
  One tag may span at most 80 KiB; the tag parser examines at most 16 MiB in total per scan.
  A tag or quoted value that runs to the end of the input ends the scan (as in an HTML parser),
  an oversized tag is skipped to the next `>`, a bare `<` in text costs one byte, and an
  exhausted budget stops the scan with a `html` diagnostic.
- Capture extraction and the DOI complement run on the daemon's network worker. A panic in one
  item is contained (`catch_unwind`): that request gets an error reply and later requests are
  still served.
- Page values are data. They are never used as commands, local paths, endpoints or file names
  (the attachment display name is derived from the final URL's last segment and restricted to
  `[A-Za-z0-9._-]`; storage is content-addressed). Unknown JSON fields in the request (e.g. a
  page-forwarded `client_kind` or `capabilities`) are ignored; authority is only the daemon grant.
- The page URL is never fetched. Network use is limited to the save operation: the DOI
  complement during preview (skippable with `offline`) and the user-selected PDF after commit.

## Authorization (security ADR)

New capability `CaptureCommit` and operations `CaptureSubmit` (needs `CapturePreview`),
`CaptureSave`, `CapturePdfStatus`, `CapturePdfRetry` (need `CaptureCommit`) and
`CaptureCancel` (either). The `native_host` preset is now `CapturePreview + CaptureCommit +
DestinationRead`; it still has no paper reads, general `ImportCommit`, merge, delete, export,
local file access, network lookup outside a capture, or job control.

- Capture previews are bound to the creating grant; another grant gets `Forbidden(CaptureSave /
  CaptureCancel)` and cannot learn whether the preview exists. Bindings expire after 330 s
  (longer than the coordinator's 300 s preview lifetime). Each grant holds at most 8; a ninth
  replaces that grant's own oldest, so one grant cannot evict another's. A daemon-wide safety
  cap of 256 evicts from the grant holding the most.
- Without `LibraryRead` duplicate matches are redacted (as for #25 import previews). Without
  `LibraryWrite` only `Create`/`Skip` are accepted; `Merge` is `Forbidden(ImportCommit)`.
  An exact duplicate therefore cannot be created by the native host (#33 rule); the user
  merges from the desktop, whose capture preview shows the matches and accepts `Merge`.
- The PDF is chosen by **index** into the preview's candidate list; the client cannot supply
  a URL at commit. `capture.pdf_fetch` is not `ipc_submittable`: `JobCommand::Submit` of that
  kind is `Forbidden(JobSubmit)` even for the desktop, so no grant can turn the daemon into a
  general URL fetcher. The job requires `CaptureCommit` from its owner grant (checked at
  claim and every heartbeat; revocation stops it, #25).
- Requested scopes only narrow and a spoofed `client_kind = "desktop"` changes nothing
  (integration-tested).

## PDF fetch policy (`cm_metadata::fetch_policy`)

| Rule | Default |
|---|---|
| Schemes | `https` only (`allow_http` opt-in also allows https→http redirects) |
| URL | no userinfo, default port only, ≤ 4 KiB, no whitespace/control characters |
| Host names | reject single-label, `localhost`, `.local`, `.localdomain`, `.internal`, `.intranet`, `.lan`, `.home`, `.corp`, `.home.arpa`, `.arpa`, `.invalid`, `.test`, `.onion` |
| Addresses | public unicast only, for IP literals (incl. decimal/hex IPv4 forms) and **every** DNS answer: no 0/8, 10/8, 100.64/10, 127/8, 169.254/16, 172.16/12, 192.0.0/24, 192.0.2/24, 192.88.99/24, 192.168/16, 198.18/15, 198.51.100/24, 203.0.113/24, multicast/reserved/broadcast; IPv6 must be 2000::/3 minus documentation, 2001::/23, 6to4 and 3fff::/20; IPv4-mapped IPv6 is judged as IPv4 |
| DNS | resolved by the policy on a helper thread, bounded by min(I/O timeout, remaining fetch budget) with cancellation polled while waiting; at most 4 lookups in flight per fetcher (more fail fast as retryable); the HTTP hop gets only the budget left after DNS; all answers checked, connection pinned to exactly those addresses (`resolve_to_addrs`); every other name is refused by the client's resolver, so no rebinding between check and connect |
| Redirects | followed manually, ≤ 5, each hop re-validated (scheme, host, port, DNS) |
| Requests | GET, `Accept: application/pdf`, `Accept-Encoding: identity`, identifying User-Agent; no cookies (no cookie store), no credentials, no `Referer`, no ambient proxy, no implicit retries |
| Response | 200 only; 401/403/407 → `AuthRequired`, 404/410 → `NotFound`; Content-Type must be PDF-compatible (`application/pdf`, `application/x-pdf`, octet-stream variants) or absent; a `Content-Encoding` other than identity → `Encoded` (own message and guidance, not retried); `%PDF-` within the first 1024 bytes |
| Size/time | 64 MiB (≤ 256 MiB) via Content-Length and while streaming; 120 s total (≤ 600 s); 15 s connect and between chunks |
| Output | streamed into a private temp file (`pdf-*.part`) under `<library>/daemon/capture-downloads/` while hashing SHA-256; leftovers from a crash are removed at daemon startup under the library lock |

Authenticated PDFs are never bypassed: no browser cookies or tokens are forwarded, and a
401/403 or an HTML (login/landing) response fails at once with guidance to download the PDF in
the browser with the user's own access and attach it explicitly (existing PDF attach flow).
Native Messaging never carries PDF bytes.

## Durable PDF job and retry

`capture.pdf_fetch {paper, url}` is submitted by the capture commit **after** the paper is
committed, in a separate step; a job failure never rolls the paper back. The commit reply
carries the job status (or `pdf_error` if submission failed — the paper is still saved).

- Idempotency key `capture-pdf:<paper>:<sha256(url)>` per owner grant: re-committing the same
  paper/URL returns the existing job.
- Effect key `(paper, sha256)`: before importing, an existing attachment with the same digest
  is reported as `AlreadyCommitted` (`detected_committed`), so recapturing the same bytes from
  another URL or a crash-rerun never adds a second attachment. Bytes are imported through #9
  `AttachmentService` with the expected digest (content-addressed, re-verified).
- Automatic retries only for transient failures (network, timeout, 408/429/5xx) with the
  engine's bounded backoff; 4 attempts; deadline 24 h. Policy blocks, redirects limits, auth,
  non-PDF, not found and size violations fail at once. Failed/cancelled jobs can be retried
  explicitly by the owner (`CaptureCommand::RetryPdf`) or a GrantAdmin (`JobCommand::Retry`).
- `Retry-After` is ignored: a 429/503 is retried on the engine's own backoff schedule, still
  bounded by the attempt limit and deadline.
- `pdf_error` in the commit reply is the user-facing (`Display`) text of the submission error.

## Known limits

- Any committed write invalidates open #33 previews (#25 note). A PDF job's bookkeeping or
  attachment write between a capture preview and its commit makes the commit fail with
  `Conflict`; the client must preview again. Clients should commit promptly; the #18 revision
  refinement is the long-term fix.
- The `loopback-test-server` feature (loopback-permitting policy, fixture server) is enabled
  only by `cm-daemon`'s dev-dependency. `scripts/check_architecture.py` rejects it on any normal
  or build dependency edge, in any default or feature table; `cargo tree -e normal,build,features`
  shows it absent from non-test builds.
- The coordinator retains at most 4 previews daemon-wide; parallel unfinished captures beyond
  that fail with a capacity error until one is committed, cancelled or expires.
- No site-specific extractors (DESIGN §8.2 step 4) and no PMID provider. Live Crossref/DataCite
  and real publisher pages are not exercised in tests (offline fixtures only).
