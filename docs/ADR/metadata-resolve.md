# #14: Read-only DOI resolution

Status: implemented against `18a1eb57597508d274dccae558a26e8dad043fd0` (#33).
Worker API only. No daemon/IPC/auth (#25), upgrade/backup gate (#23), packaging (#34),
GUI, browser, PDF download/open, or BibTeX/RIS/export (#13) integration is introduced.

## Composition and confirmation

```rust,ignore
use cm_core::{metadata_resolve::{Cancellation, MetadataResolveService},
              import::{ImportCoordinator, ImportAction, ImportCommit, ImportDecision},
              ports::ImportService};
use cm_metadata::doi::{HttpConfig, HttpProviders};
// Construct and call outside async runtimes, on a dedicated synchronous worker.
let mut resolver = MetadataResolveService::new(HttpProviders::new(HttpConfig::default())?);
let resolved = resolver.resolve("https://doi.org/10.1234/example", &Cancellation::default())?;
// Inspect outcomes, field evidence, source hashes, missing fields and PDF candidates.
// If manual_add, edit resolved.draft (especially its blank title) and preview again.
let mut imports = ImportCoordinator::new(db.imports());
let preview = resolved.preview(&mut imports, Default::default())?;
// Show common exact/fuzzy duplicates and differences. Only after explicit approval:
let approved = ImportCommit { preview_id: preview.id,
    decisions: vec![ImportDecision { candidate_index: 0,
        action: ImportAction::Merge { match_index: 0, fields: vec!["title".into()] } }] };
let outcome = imports.commit(approved)?;
```

`MetadataProvider` and `MetadataResolveService` live in framework-free core. They have no
DB/file handle and no commit method. `HttpProviders` owns reqwest/Tokio in metadata. The
neutral `PaperDraft` enters the **existing** #33 coordinator; normalization, duplicate
priority, citation keys, field diffs, revision/decision validation, cancellation and SQLite
commit are not reimplemented. `MetadataResolution::preview` is only a forwarding helper.
A manual fallback carries the canonical DOI, a blank title and diagnostics; users can
complete the draft and request a fresh common preview. No successful title is fabricated.

## Identity, provenance and partial/conflicting fields

- Only `papers::normalize_doi` defines identity. Input is at most4096 UTF-8 bytes. The
  request and every response DOI (including DataCite `data.id` and DOI identifiers) must
  have the same current canonical identity. Mismatches discard the entire provider record.
  Raw suffix percent literals stay distinct from decoded resolver URL suffixes. URI path
  segment construction percent-encodes the **whole** DOI, including slashes, percent signs,
  question marks and fragments; callers cannot supply an endpoint/host/path template.
- Crossref is first. DataCite is tried on any primary failure or incomplete primary record.
  A primary is complete for this policy only when title, authors, year, publication_date,
  type, abstract, journal, volume, issue, pages, URL, language **and** a PDF candidate are
  present. Complete Crossref results skip DataCite explicitly (`NotNeeded`). Many valid
  records lack an issue/PDF; those intentionally receive one complement lookup.
- Choose the first nonblank/nonempty field, in provider order. A later provider only fills
  absent fields. Ordered contributors are one field, never concatenated across providers.
  Date/year are a consistency group: a different secondary year cannot supply a date to a
  primary year. Existing primary date precision is retained, even if the secondary is more
  precise. Field evidence retains both values and identifies which provider supplied each
  selected field; a conflict never overwrites the primary. Common normalization happens
  in #33 after this provider-selection policy.
- Crossref preserves author order, then editor order, Unicode, literal organizations and
  ORCID; `issued`, then `published-print`, then `published-online` supplies exact year/month/
  day precision. DataCite preserves creators followed by contributors, organizational
  names, ORCID, lowercase contributor roles, `Issued` precision or publication-year-only.
  No person-name splitting, transliteration, date day/month inference or abstract HTML
  rendering is performed. Main title, type, abstract, journal/container, volume/issue/pages
  (or Crossref article number), DOI, landing URL and language map to shared bibliography.
- Known external IDs are compared using #33's `external_id` function. Canonical repeats
  within a provider are coalesced before common preview (DataCite can repeat its primary
  DOI), retaining the first raw spelling. All raw external-ID spellings, provider-specific
  unmapped metadata, alternate titles, source endpoint and SHA-256 of the exact JSON bytes
  remain under `extra_json.doi_resolution.sources` / `raw_external_ids` plus field evidence.
  Known IDs in the **common preview** use #33's canonical spelling; unknown namespace/value
  spelling remains unchanged. No provenance table/migration is added. `extra_json` is saved
  only by an explicitly approved Create or selected merge field; unselected existing
  provenance, DOI and raw IDs stay verbatim.
- Legacy compatibility and ambiguity remain exactly [#33's policy](import-service.md#legacy-doi-compatibility-p1-repair).
  A retained old DOI/raw resolver witness matches the new identity without rewriting it.
  An unwitnessed historical escaped value is indistinguishable from an intentional percent
  literal, so no decoded alias is guessed. Multiple exact persisted matches stay visible;
  the coordinator rejects colliding creates/merges rather than picking a record.
- Failures have explicit provider status and attempt/cache/retry advice. `manual_add` means
  no usable title, rather than "every bibliographic field is populated". Successful partial
  results remain usable while failed provider diagnostics survive. If combined evidence
  exceeds the common64 KiB row/count budget, return a small diagnostic manual draft with
  `TooLarge` outcomes for discarded found records; never silently truncate metadata.

## HTTP, resource bounds and retry/cache rules

- Fixed public HTTPS singleton endpoints: `https://api.crossref.org/works/{doi}` and
  `https://api.datacite.org/dois/{doi}`. GET only, Accept JSON, no credentials/API key/mailto,
  no ambient proxy, no cookie feature, no arbitrary endpoint configuration. Redirect limit
  **zero**, including same-host redirects; return `RedirectRejected` without following.
  Alternate loopback endpoints exist only in `cfg(test)` private unit-test state.
- Identifying configurable public application User-Agent defaults to
  `citation-manager/0.1 (+https://github.com/ks250206/citation-manager)`. Configuration
  validates length10–512, application/version and public HTTPS identifier, no controls.
  No user email or credentials are invented. Request bodies, private library contents,
  provider metadata, headers and URLs are never logged by this implementation.
- Per-attempt connect/full-body timeout defaults3s, configurable >0 through10s. Attempts
  default2, maximum3 per provider. Retry only network/timeout,429 and5xx; no retry for404,
  malformed schema, DOI mismatch, size violation, other4xx or redirect. Reqwest's implicit
  protocol retries are disabled, so the attempt count is authoritative.
- Sequential per-provider minimum spacing250ms. Honor valid Crossref
  `X-Rate-Limit-Limit` / `X-Rate-Limit-Interval` when slower. Exponential retry backoff starts
  at250ms and doubles.429/503 honor `Retry-After` integer seconds **or HTTP-date**, also
  respecting minimum spacing/backoff. Maximum inline wait2s; a longer delay returns retry
  advice immediately and blocks later fetches on that adapter until its full deadline.
  Long server delays are **not** clamped into early retries. Rate state is per instance;
  callers should reuse one worker adapter. This is not a process-global traffic quota.
- `Cancellation` is local, one-shot, cloneable atomic state, not a job/auth envelope.
  Cancellation stops new attempts/cache reads and is checked at most10ms apart during
  transport/wait. It drops in-flight response futures, retains prior successful partial
  results with explicit cancelled provider outcomes, and never caches cancelled requests.
  Retry uses a fresh token; there is no automatic background/resume job. Call common
  `ImportService::cancel` separately to discard an already-created import preview.
- Stream body cap defaults/max256 KiB, configurable downward; declared length is checked
  before allocation, and every chunk against the cap. Only200 JSON / application `+json`
  is parsed. Error/redirect bodies are not consumed. Strict typed singleton envelopes reject
  wrong field types, malformed JSON, invalid dates and missing identities; unknown fields
  remain bounded by the body/common draft limits. No search/list endpoint is used.
- Per record: total contributors256; provider PDF links32; titles32; DataCite dates and
  descriptions32 each, name identifiers32 per contributor, alternate IDs127. Crossref
  date-parts is exactly one1–3-component date. Common64 KiB draft,128 IDs and diagnostic
  limits also apply. Result PDF candidates cap32, deduplicated by URL.
- Memory-only cache keyed by `(Provider, current canonical DOI)` for this adapter/schema
  instance; no raw input URL/query, user/library identity or persistence. At most64 entries
  (configurable1–64), FIFO eviction, expired entries pruned before lookup. Found TTL defaults/
  max600s;404 TTL defaults/max30s, measured monotonically from completion. Either TTL can
  be zero to disable that class. No transient, schema/identity, redirect or cancellation
  failure is cached. Cached hits have attempts0 and cached=true; rate/cancellation still
  remain separate outcomes. There is no public API availability requirement in CI.

## PDF candidates

Crossref `link` entries must declare `application/pdf`; DataCite `contentUrl` must have a
`.pdf` path. Candidates carry provider and exact metadata-response source/hash. Their
URLs must parse as HTTP/HTTPS, have no userinfo or unexpected port, and pass a conservative
public-host syntax check (reject local/private/reserved IPs, single-label/local/internal
names and invalid/test suffixes). This is **not** DNS/reachability/access/license proof.
No PDF URL is requested, opened, downloaded, attached or scheduled here. A future authorized
fetcher must independently check DNS, redirects, access rights and file policy at fetch time.

## API evidence and validation

`find-docs` resolved `/seanmonstar/reqwest`, then queried the pinned `/v0.13.5` documentation.
Pinned local source confirmed `ClientBuilder::{timeout,connect_timeout,redirect,retry,no_proxy}`,
`retry::never` and `Response::chunk`; the exact pinned adapter compiled and executed against
loopback HTTP. Existing workspace reqwest0.13.5 JSON/rustls features remain unchanged;
Tokio1.54.1 remains pinned, with its time feature used on the worker. Direct exact
httpdate1.0.3 parses standard Retry-After HTTP-dates rather than guessing calendar math.

Official references: [reqwest0.13.5](https://docs.rs/reqwest/0.13.5/reqwest/),
[Crossref REST singleton schema and headers](https://github.com/CrossRef/rest-api-doc),
[Crossref access and rate headers](https://www.crossref.org/documentation/retrieve-metadata/rest-api/access-and-authentication/),
[DataCite public API and Findable-only visibility](https://support.datacite.org/docs/rest-api),
[DataCite response metadata example](https://support.datacite.org/docs/api-create-dois).
A404 is provider NotFound, not proof that a DOI is unregistered globally or nonexistent.
See [executed evidence and limits](../validation/ISSUE_14.md).
