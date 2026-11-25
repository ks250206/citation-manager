# #27 validation: authenticated local REST API and OpenAPI contract

Date: 2026-11-25. Linux (container), rustc 1.99.1, private target dir with
`CARGO_INCREMENTAL=0`, dev/test debuginfo 0. Design: [rest-api ADR](../ADR/rest-api.md).
Contract: [openapi.json](../openapi.json).

## Automated (Linux)

| Area | Test | What it proves |
|---|---|---|
| Contract | `cm-daemon rest::openapi::tests::router_openapi_and_scope_table_list_the_same_endpoints` | router routes = OpenAPI operations = `ENDPOINTS` REST rows, both directions |
| | `documented_scopes_match_the_shared_table`, `schema_references_resolve`, `openapi_document_is_committed` | `x-required-scopes` = table; 401/403 documented for every scoped row; refs resolve; committed file = generated |
| Edge units | `rest::edge::tests::host_allowlist_is_exact`, `rest::ctx::tests::*`, `rest::tests::origins_are_bare_scheme_host_port`, `rest::handlers::tests::export_file_names_are_plain` | Host allowlist, strict bearer parsing (one header), URL-safe cursors, origin syntax, plain export names |
| Core | `cm-core papers::tests::paper_patch_distinguishes_absent_null_and_value_and_round_trips`; existing 1024-combination scope tests (with `ImportCancel` added to `POST /imports`) | merge-patch semantics; table/authorizer agreement |
| Daemon (`daemon_rest`, real papersd on an ephemeral loopback port) | `every_endpoint_requires_a_valid_rest_token` | every row: no token / `Basic` / unknown token → 401 with `WWW-Authenticate`; owner desktop credential → 403 `token_not_allowed`; health public |
| | `every_endpoint_enforces_exactly_its_scopes` | every row × every required scope removed → 403 with `required_scopes`; bulk rows without elevation → `elevation_required`; exactly the required scopes pass |
| | `revoked_tokens_are_refused_everywhere_including_live_keep_alive_connections` | revoke between two requests on one keep-alive connection → 401; every row 401 after revoke; narrowing applies to the next request |
| | `invalid_bodies_are_rejected_on_every_body_endpoint` | malformed JSON → `invalid_json`, wrong media type → 415, unknown field → 400, unauthenticated bad JSON → 401 (body read only after auth) |
| | `spoofed_host_origin_and_cross_site_requests_are_refused_everywhere` | every row: foreign/rebinding/port-mismatch/portless Host → 400; foreign, `null` and self Origins → 403 without CORS headers; `Sec-Fetch-Site: cross-site` → 403; 404/405 standard errors |
| | `a_configured_origin_gets_cors_and_others_do_not` | allowlisted origin gets preflight + ACAO, still needs a token |
| | `every_endpoint_normal_case_matches_the_contract` | every row exercised successfully (render: 503 `pdf_runtime_unavailable` without PDFium; DOI: input rejection without network); cursor paging; import dry run/commit with duplicate skip; export into the allowed folder, no overwrite; job get/cancel; other owner's job 404; bulk with elevation; audit records the REST write as an `api` grant |
| | `limits_and_path_restrictions` | limit/cursor/unknown query/snippet/timeout/q length/page and character ranges → 400; 1 MiB body → 413; >500 ids, duplicates → 400; >2 MiB import → 413; long URI → 414; export `../`, absolute, dot, separator names → 400; foreign or `..` folder → 403; symlink target refused; no folders → 403 |
| | `lower_scopes_never_see_full_text_or_annotation_content` | papers:read (and two of three read scopes) get 403 for search/full text/annotations with no marker text in the body; write-only PATCH responses omit records; import preview for write-only hides the matched id |
| | `rest_and_ipc_agree_for_the_same_grant` | same token over REST and IPC (the CLI/MCP path): identical paper list, paper, search hits (ids, order, snippets, scores, pages) and identical denials |
| | `the_request_log_has_no_tokens_queries_or_content` | log lines carry id/method/route template/status/duration only; no token, title, query, body text, Host or DOI |
| | `the_listener_is_off_by_default_and_follows_the_settings` | default off (nothing bound); enable → listening; port change moves it (old port closed, Host follows); disable → closed |
| | `timeouts_connection_and_concurrency_limits` | slow headers cut off; connections over the limit closed; request deadline → 504; no free slot → 503 + `Retry-After` |
| | `shutdown_with_the_api_enabled_releases_the_library_promptly` | shutdown drains REST sessions before the worker exits and the lock is released; a new daemon serves the API at once |
| | `papersd_binary_serves_the_persisted_api_on_start` | real binary: persisted enabled API listens on start and pins the daemon past its idle timeout |

Every response in `daemon_rest` is checked against `docs/openapi.json`: its status must be
documented for that endpoint, its body must validate against the schema, and errors carry
the same request id as `X-Request-Id`.

## Manual curl smoke (real `target/debug/papersd`)

`CM_SECRET_STORE=file`, temp library. `papersd api set --library L --enable --port 39555`
(started the daemon through the connector), `papersd token issue ... --scope papers:read` and
`--scope papers:write` (`--allow-non-tty`, tokens kept in private temp files):

| Request | Result |
|---|---|
| `GET /api/v1/health` (no auth) | `200 {"status":"ok"}`, `x-request-id: req_…` |
| `GET /api/v1/papers` (no auth) | `401 unauthenticated`, `www-authenticate: Bearer realm="citation-manager"` |
| `GET /api/v1/search?q=test` with papers:read | `403 forbidden`, `required_scopes: [papers:read, files:read, annotations:read]` |
| `POST /api/v1/papers` with papers:read | `403 forbidden`, `required_scopes: [papers:write]` |
| `POST /api/v1/papers` with papers:write | `201`, `location: /api/v1/papers/<id>` |
| `GET /api/v1/papers?limit=5` with papers:read | `200` with the new paper |
| `Host: evil.example:39555` | `400 host_not_allowed` |
| `Origin: https://evil.example` on `/health` | `403 origin_not_allowed` |
| after `papersd token revoke` | `401` |
| after `papersd api set --disable` | connection refused; papersd idle-exited ~30 s later |

## Not verified

macOS (`.app`, launchd, Keychain) (#34); the GUI listener hint was compiled and
clippy-checked (`--features desktop-bootstrap`), not operated; rendering success with a real
PDFium and DOI resolution against the network over REST; multi-hour soak/load.
