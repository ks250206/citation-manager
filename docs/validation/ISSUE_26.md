# #26 validation: tokens, scopes, MCP profiles, elevation, API settings

Date: 2026-11-25. Linux (container), rustc 1.99.1, private target dir with
`CARGO_INCREMENTAL=0`, dev/test debuginfo 0. Design: [api-tokens-grants ADR](../ADR/api-tokens-grants.md).

## Automated (Linux)

| Area | Test | What it proves |
|---|---|---|
| Scope table | `cm-core policy::scopes::tests::every_scope_combination_matches_the_endpoint_table` | all 1024 scope subsets × every REST/MCP/CLI row: shared authorizer decision == documented scopes |
| | `every_operation_requirement_is_minimal_per_scope`, `owner_only_and_native_host_capabilities_are_never_reachable_by_scopes`, `mcp_read_only_profile_allows_exactly_the_reads_of_the_25_preset`, `lifetime_policy` | each scope needed; owner/native-host capabilities unreachable; native-host preset unchanged; expiry policy |
| Elevation | `policy::elevation::tests::*` | per-grant sliding window, elevation lifts it, ≤15 min, needs a base scope; `elevatable_kinds` == accepted kinds |
| Settings | `policy::api::tests::*`, `cm-storage daemon::tests::api_settings_default_off_round_trip_and_constraints` | off by default, loopback only, port ≥1024, DB CHECKs |
| Migration | `cm-storage migrations::tests::issue_26_carries_legacy_api_tokens_over_revoked_and_marks_the_desktop_grant` | `api_tokens` rows become revoked, capability-less grants; table dropped |
| Secret store | `cm-protocol client::secrets::tests::*` | 0600/0700 file fallback, widened file refused, name/secret validation |
| Client deadlines | `cm-protocol client::tests::secret_store_commands_get_a_deadline_covering_every_helper_run`, `a_slow_secret_store_reply_outlives_the_control_deadline` | issue/re-issue/revoke deadlines cover every secret-store helper run; a slow reply is delivered, not discarded |
| Daemon (`daemon_api`) | `scoped_tokens_follow_the_table_and_secrets_stay_out_of_the_database_and_logs` | end-to-end scope decisions; no token/desktop secret in any persisted file (DB, WAL, audit); audit has no titles/labels |
| | `revocation_and_scope_changes_reach_live_sessions_immediately` | widening and narrowing apply to an open session; revoke → `Denied(Revoked)`, handshake rejected, secret deleted |
| | `reissue_rotates_the_secret_and_revokes_the_old_token` | rotation semantics |
| | `read_only_profiles_cannot_widen_themselves` | spoofed kind/all-scope narrowing, self `SetScopes`/`IssueToken`/`Elevate`/`GrantCommand::Issue`, settings, audit, export all refused; narrowed owner cannot widen |
| | `bulk_operations_need_a_short_explicit_elevation` | 11th trash refused as `BulkTrash`; 16-min, read-only, permanent and scope-issued elevation refused; 1-min elevation works, ending it restores the limit; audit entry |
| | `a_delete_and_write_token_can_be_elevated_for_bulk_moves` | `papers:delete`+`papers:write` token: 26th move refused as `BulkMove`, a move elevation lifts moves only |
| | `export_tokens_write_only_inside_allowed_folders` | symlinked folder refused in settings; exports only inside allowed folders |
| | `api_settings_and_tokens_persist_across_restart` | settings/tokens/last-use survive restart; pre-#26 owner grant upgraded in place |
| | `cli_bootstrap_prints_a_token_once_and_only_where_allowed` | non-TTY refused before issuing (exit 3); `--allow-non-tty` prints one line, secret not on stderr; list/`api set` |
| Daemon audit | `audit::tests::rotates_and_stays_bounded` | two files at most, 0600 |
| Desktop | `tests/api_settings.rs`, `commands_keymap::api_settings_provider_is_registered_and_connected_by_its_host` | form/expiry validation, show-once secret redacted in Debug, grouping, MCP toggle, elevation kind choice (only allowed kinds offered, chosen kind sent); `api.*` registered, not deferred |

## GUI under Xvfb (Linux, lavapipe)

Built `cargo build -p cm-daemon --bins --features desktop-bootstrap`, installed with
`scripts/install_linux.py --prefix <tmp> --executable target/debug/citation-manager`
(pinned PDFium downloaded and verified), run on a private Xvfb display, driven with xdotool.
No OS keyring/session bus was present, so the file fallback and its warning are shown.

1. [Palette](issue-26/01-palette-api-commands.png): `api.open_settings`, `api.new_token`,
   `api.new_mcp_profile` listed and enabled.
2. [Defaults](issue-26/02-api-settings-default-off.png): API off, 127.0.0.1, port 39271, file
   store warning, listener-not-in-build notice.
3. Issued a REST token (papers:read/write, files:read): secret shown once with Copy/Done
   (screenshot not kept because it shows a secret); `Done` removes it; row shows 30-day expiry.
   After restarting the GUI and daemon the token was still listed.
4. Created a read-only MCP profile; [Allow writes warning](issue-26/03-mcp-allow-writes-warning.png),
   confirmed; `papersd token list` against the same daemon showed the new scopes.
5. [Saved settings](issue-26/04-settings-saved-loopback-v6.png): On, `::1`, port 40100;
   `papersd api show` agreed.
6. [Port 80 refused, revoke confirmation](issue-26/05-port-refused-revoke-confirm.png).
7. [Revoked token, MCP with writes, audit entries](issue-26/06-revoked-mcp-writes-audit.png).
8. `papersd token issue … > /dev/null` against the GUI's daemon: refused, exit 3.

## Review follow-up (2026-11-25)

Fixes for the independent review, each with a regression test:

| Item | Test |
|---|---|
| Owner-only `ApiAdmin`/`ElevationAdmin`/`GrantAdmin`/`BulkElevated`; not delegable; delegated grants always expire | `daemon_api::delegated_grants_never_get_owner_powers_or_unlimited_lifetimes`, narrowed-owner checks in `bulk_operations_need_a_short_explicit_elevation` and `export_tokens_write_only_inside_allowed_folders`, `scopes::owner_only_and_native_host_capabilities_are_never_reachable_by_scopes` |
| Audit: coalesced denials with counts, separate admin sink, unsafe file moved aside and reported | `audit::tests::denials_are_coalesced_and_cannot_evict_admin_records`, `audit::tests::an_unsafe_log_is_moved_aside_and_reported` |
| Bulk guard: annotation deletes count, monotonic window/elevation, one kind per elevation | `elevation::tests::*`, `ipc::tests::annotation_deletes_count_toward_the_delete_burst`, `daemon_api::bulk_operations_need_a_short_explicit_elevation` |
| Export folders: home/library/state/ancestors refused, dev/ino containment, extension, caps | `export::tests::home_library_and_its_ancestors_are_refused_as_export_folders`, `ipc::tests::export_containment_uses_the_real_directory_and_the_format_extension`, `daemon_api::export_tokens_write_only_inside_allowed_folders` |
| Secret store: no-follow read, fallback outside the library, helpers always reaped | `secrets::tests::file_store_is_private_round_trips_and_refuses_odd_names` (symlink case), `local::tests::secret_fallback_lives_outside_the_library` |
| Admin CLI: undelivered token revoked (exit 1), control characters stripped | `daemon_api::cli_bootstrap_prints_a_token_once_and_only_where_allowed` (stdout = `/dev/full`), `admin::tests::labels_lose_control_characters` |
| GUI: zeroizing wrapper, masked, sequence numbers, draft kept | `desktop tests/api_settings.rs` (`stale_list_replies_are_recognised_by_sequence`, draft and mask cases) |
| Endpoint rows for job submit/retry | covered by the scope-combination test |

GUI re-run on a private Xvfb display (`--skip-pdfium` install): a new secret is
[masked by default](issue-26/07-secret-masked-by-default.png); after Copy the X clipboard held
64 characters (`xclip -o -selection clipboard`), after Done it held 0; a second copied token
was cleared from the clipboard after 30 s; an unsaved On/port draft
[survived a refresh](issue-26/08-unsaved-draft-kept-on-refresh.png); file-fallback copies
were created under `~/.local/share/citation-manager/secrets/<library key>/`, not in the
library. Not exercised in the GUI: the `Issued`-after-leaving-the-screen revocation (timing),
the Reissue rollback path (needs a failing revoke) and the unsafe-audit warning display.

## Not verified

macOS Keychain (`security -i`) and Linux Secret Service (`secret-tool`) backends, and
keychain ACL prompts between `papersd`, the GUI and `papers`; clipboard contents of Copy
(button pressed, clipboard not read back); the REST listener itself (#27); MCP/CLI clients
loading their token from the store (#28/#29).
