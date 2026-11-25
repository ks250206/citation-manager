# #23: Browser Native Messaging host, registration and message contract

Status: implemented on Linux and verified with real Chromium and Firefox ESR against the real
#25 `papersd` ([ISSUE_23](../validation/ISSUE_23.md)). macOS paths are implemented from the
documented browser locations but not run; Windows is not supported. The browser extension
itself is #24; the `.app` bundle and its launch hooks are #34.

## Process and placement

`papers-native-host` (`crates/native-host`, depends only on `cm-protocol`) is installed as a
sibling of `papersd`: `<prefix>/bin/` on Linux (`scripts/install_linux.py`),
`Citation Manager.app/Contents/MacOS/` on macOS ([app layout](app-layout.md)).

- The browser starts it per `runtime.sendNativeMessage` (one message) or per
  `runtime.connectNative` port. It answers messages sequentially and exits when the browser
  closes stdin, when an open port has been idle for 60 s, or after a framing error.
- Each daemon request uses a fresh IPC session that is closed right after the reply, so the
  host never keeps `papersd` alive. If the daemon is not running it is started exactly like
  the GUI does (#25 `LocalConnector`): the absolute `papersd` next to the host's own
  executable (`current_exe`), never a PATH search, in its own process group with closed stdio.
  The daemon therefore outlives the host, the browser and the GUI, and finishes its durable
  jobs (e.g. `capture.pdf_fetch`) before idling out.
- Daemon I/O is bounded per operation: 15 s to start `papersd`, 10 s for each handshake
  read/write, and 300 s for each later socket read or write (a reply is one read; there is no
  separate total deadline per request).
- stdout carries only protocol frames. stderr carries one diagnostic line per message
  (`papers-native-host: capture.preview: ok (1130 bytes in)`, error codes, framing problems,
  rejected caller IDs). Page content, URLs, titles, previews and credentials are never written.
  Chrome forwards host stderr to its own stderr log; Firefox to the browser console.
- Exit status: 0 port closed/idle, 1 stdout gone, 2 not started by a browser (usage),
  3 caller not allowed or host not registered (nothing is read or written on stdout),
  4 oversized/truncated frame (after an error reply).

## Authority: a daemon-issued capture grant, not a proxy

The host authenticates with its own credential for a daemon grant holding exactly the
`native_host` preset: `CapturePreview + CaptureCommit + DestinationRead` (#22/#25). It also
requests that set in `Hello`, so a broader grant would still be narrowed. It never reads the
owner's desktop credential when started by a browser.

The grant is obtained by `papers-native-host pair` (or `register --pair`), which the user or
installer runs explicitly: it opens the library as its owner (desktop bootstrap credential,
starting `papersd` if needed), calls `GrantCommand::Issue{label: "browser native host"}` and
stores the secret 0600. Re-pairing with a still valid credential is a no-op; a replaced grant
is revoked; `unregister` revokes it. A settings UI with consent and listing replaces this CLI
step in #26.

No grant id is ever forgotten while the grant may still be live:

- `pair` records a newly issued grant in `config.json` (`revoke_pending`) *before* storing its
  secret. If storing the secret or the config fails, the new grant is revoked again (or stays
  recorded as pending); the previous pairing is untouched. A replaced grant goes to
  `revoke_pending` and is revoked; a failed revocation stays pending and is retried by the
  next `pair` or `unregister` (the command exits 1 and says so).
- `unregister` removes the manifests first, then revokes. If the daemon cannot be opened or a
  revocation fails, the config is kept, marked `unregistered` (the host then refuses every
  browser launch), with the ids in `revoke_pending`; only the no-longer-needed secret is
  deleted. It exits 1 and asks to run `unregister` again; a successful run deletes everything.
- `unregister --keep-grant` removes only the manifests and marks the config `unregistered`,
  keeping config and credential and contacting no daemon; `register` restores browser access
  with the same grant, a plain `unregister` revokes it.
- `pair` requests no explicit expiry. Since #26 every delegated grant expires
  ([api-tokens-grants](api-tokens-grants.md): `CaptureCommit` is a write capability, so the
  native-host grant gets the write lifetime, 30 days by default); once it has expired the
  browser gets `unauthorized` and a plain `pair` (no `--force` needed) issues a new grant and
  revokes the old one. Before that it ends by explicit revocation (`unregister`, re-pairing,
  or a GrantAdmin / the #26 "API & integrations" settings), and failed revocations are tracked
  as above. Registering another library while a grant is held is refused until
  `unregister` revoked it in its own library.

Browser messages map to a closed set of commands (`message::Request` -> `daemon::command`):
`Organization(List)` and `Capture(Preview | Commit | Cancel | PdfJob | RetryPdf)`. There is no
pass-through of `Command` JSON; anything else is `unknown_type` and never reaches the daemon.
The daemon remains the authority (merges, paper reads, general commits, job submission and
grants are denied by the grant, as the smoke test shows for `Merge`).

## Caller validation (defence in depth)

The browser only starts hosts whose manifest allows the extension. The host checks again:

| Browser | argv | Accepted when |
|---|---|---|
| Chrome/Chromium | `chrome-extension://<id>/` (+ `--parent-window=<n>` on Windows) | `<id>` is 32 × `a`–`p` and in the Chrome allowlist |
| Firefox | `<absolute manifest path>.json <extension id>` | gecko ID (`name@domain` or `{GUID}`, ≤ 80 bytes) in the Firefox allowlist, and the manifest path is one `register` wrote (compared after resolving symlinks) |

Anything else exits 2/3 without reading stdin, as does any launch while the config is marked
`unregistered`. IDs of one browser never match the other's list.

## Registration

Per-user state lives in `<config>/native-host/` (0700): `config.json` (0600: library,
optional runtime directory, host path, allowlist, manifest records, grant id) and
`credential` (0600). `<config>` is `$XDG_CONFIG_HOME/citation-manager` (else
`~/.config/citation-manager`) on Linux and `~/Library/Application Support/Citation Manager` on
macOS. Nothing is read from the browser or page to locate it.

Host name `dev.citation_manager.native_host`. Manifest locations (per user):

| Browser | Linux | macOS |
|---|---|---|
| Chrome | `~/.config/google-chrome/NativeMessagingHosts/` | `~/Library/Application Support/Google/Chrome/NativeMessagingHosts/` |
| Chromium | `~/.config/chromium/NativeMessagingHosts/` | `~/Library/Application Support/Chromium/NativeMessagingHosts/` |
| Firefox | `~/.mozilla/native-messaging-hosts/` | `~/Library/Application Support/Mozilla/NativeMessagingHosts/` |

`--browser NAME=DIR` writes to another directory (e.g. a custom Chromium `--user-data-dir`'s
`NativeMessagingHosts/`, or Brave/Edge with the Chrome format). Chrome manifests use
`allowed_origins`, Firefox manifests `allowed_extensions`.

- `register --library ABS [--runtime-dir ABS] [--browser NAME[=DIR]]... [IDs]` writes the
  config and manifests atomically (0644). `path` is always the canonical absolute path of the
  running executable. It refuses an executable inside a source checkout's `target/` directory
  (unless `--allow-development-build`), one that is not a regular executable, or one (or its
  directory) that is group/other-writable or not owned by the user/root, and requires an
  executable `papersd` sibling, and a valid UTF-8 path (JSON manifests cannot carry anything
  else). Without `--browser` it registers the browsers whose profile directory exists.
  Re-running keeps earlier manifests, IDs and pairing. A browser family without any allowed ID
  gets no manifest (an empty `allowed_origins` is useless; a stale one of ours is removed).
- A `config.json` that exists but is unreadable, unsafe (not 0600/0700) or corrupt is an error
  for every subcommand; it is never treated as "not registered" (which would overwrite it or
  lose the grant id inside). Only a missing config means unregistered.
- `refresh` re-points every recorded manifest when the executable moved or was replaced. It
  skips records whose browser profile root (the manifest directory's parent) no longer exists
  and never recreates it, is a no-op when unregistered or already current, and never contacts
  the daemon, so the app (#34: on every `.app` launch) and installers can call it
  unconditionally. `scripts/install_linux.py` runs `register` when it has extension IDs and
  otherwise `refresh`, so re-running it after moving or reinstalling the prefix always
  re-points an existing registration.
- `unregister [--keep-grant]`: see above; it only removes files carrying our host name.
  `status` prints JSON and exits 1 when a manifest is missing or stale, the config is marked
  `unregistered`, or revocations are pending. `manifest --browser NAME [--host-path ABS]` prints one document for
  packaging.

### Extension IDs

`browser-extension/native-manifests/extension-ids.json` lists `development` and `release` IDs.
The development extension (`dev-extension/`, used only by the smoke test) has the Chrome ID
`baapgkfjfjjagdkpkmocjhfopmaohcdc`, derived from the public `key` in its manifest (the private
key was discarded, so the ID cannot be published), and the gecko ID
`citation-manager-dev@citation-manager.invalid`. They are allowed only with
`--development-extensions`, and `register` prints a warning when they are: these IDs are
**claimable** by anyone who loads an unpacked (Chrome developer mode, using the committed
public key) or temporary (Firefox) extension, so they must never be registered on a machine
used for real captures. They only stop a store-published extension from using them. #24 adds the store IDs under `release`; the installer passes them
as `--chrome-extension-id`/`--firefox-extension-id`. Until then the Linux installer registers
nothing new unless `--development-extensions` is given (it still refreshes an existing
registration).

## Message contract (version 1)

Framing (both browsers): 4-byte length in native byte order + UTF-8 JSON. Browser -> host is
limited to 8 MiB by the host (browsers allow 64 MiB; a #22 capture is ≤ 4 MiB of page data and
must fit the 8 MiB IPC frame); a larger declared length is answered `too_large` without
allocating and the host exits. Host -> browser is at most 1 MiB; a larger result becomes
`response_too_large`.

```text
request:  {"v": 1, "id": <string 1..=64 [A-Za-z0-9._:-] | integer | null>, "type": "<type>", ...}
success:  {"v": 1, "id": <echo>, "ok": true,  "type": "<type>", "result": {...}}
failure:  {"v": 1, "id": <echo|null>, "ok": false,
           "error": {"code": "<code>", "message": "<text>", "retryable": <bool>, "detail"?: {...}}}
```

| type | fields | daemon command | result |
|---|---|---|---|
| `hello` | – | none (does not start the daemon) | `{host_version, protocol, browser, paired, types, limits}` |
| `destinations` | – | `Organization(List)` | `{collections:[{id,parent,name,sort_order}], tags:[{id,name,key,color}]}` |
| `capture.preview` | `capture`: #22 `CaptureRequest` (`page_url`, `title?`, `html?`, `meta[]`, `json_ld[]`, `pdf_url?`, `destination{collection?,tags[]}`, `offline`) | `Capture(Preview)` | `CapturePreview` (`preview.id`, `preview.candidates`, `resolution.pdf_candidates`, diagnostics) |
| `capture.commit` | `preview_id`, `decisions[1..=16]` (`{candidate_index, action: "Create"|"Skip"}`), `pdf?` (index into `resolution.pdf_candidates`) | `Capture(Commit)` | `CaptureCommitted` (`paper`, `outcome`, `pdf_job?`, `pdf_error?`) |
| `capture.cancel` | `preview_id` | `Capture(Cancel)` | `{cancelled: true}` |
| `capture.pdf_job` | `job` (UUID) | `Capture(PdfJob)` | `JobStatus` (`state`, `attempts`, `error`, ...) |
| `capture.retry_pdf` | `job` | `Capture(RetryPdf)` | `JobStatus` |

Result objects are the `cm-protocol` serde shapes (Rust enums externally tagged, e.g.
`"Create"` / `{"Merge": {...}}`). Unknown fields are ignored, including page-forwarded
`client_kind`/`capabilities`. Versioning: additive fields and new types keep `v: 1`; removing or
changing the meaning of a field or type bumps `v`, and a host answers other versions with
`unsupported_version` and `detail.supported`. The daemon IPC version (#25) is independent.

| code | retryable | meaning / extension action |
|---|---|---|
| `bad_message` | no | not UTF-8 JSON, not an object, bad `id`, missing `v`/`type`, wrong field shapes, malformed frame |
| `unsupported_version` | no | `detail.supported` lists host versions |
| `unknown_type` | no | not one of the types above (`detail.supported`) |
| `too_large` | no | request over the host/daemon limit (host exits after an oversized frame) |
| `response_too_large` | no | result over 1 MiB |
| `not_paired` | no | no capture grant stored: ask the user to pair (`papers-native-host pair` / app settings) |
| `unauthorized` | no | grant unknown, revoked or expired: pair again |
| `denied` | no | not allowed for the capture grant (`detail.operation`), e.g. Merge |
| `invalid_input` | no | daemon validation message (no page content echoed by the host) |
| `conflict` | yes | the library changed or the preview expired: preview again |
| `not_found` | no | preview/job unknown to this grant |
| `busy` | yes | daemon busy |
| `cancelled` | no | request cancelled |
| `daemon_unavailable` | yes | daemon could not be started/reached or went away |
| `daemon_rejected` | no | other library/peer check/IPC version mismatch (`detail.daemon_supported`) |
| `internal` | no | unexpected reply |

## PDFs never travel through Native Messaging

No message carries PDF bytes, and `capture.commit` cannot name a URL: it selects a candidate by
index from the daemon's own preview. The daemon fetches public PDFs itself in the durable
`capture.pdf_fetch` job with the #22 SSRF-safe policy, after the paper is committed and after
the host has exited. The extension polls `capture.pdf_job` (or the GUI shows it) and can retry
with `capture.retry_pdf`. PDFs that need the user's browser session are downloaded by the user
and attached explicitly in the app (#22).

## What #24 calls

1. `hello` on install/start-up: check `protocol == 1`, `paired` (otherwise show the pairing
   instruction). It never starts the daemon.
2. `destinations` for the picker; put chosen IDs in `capture.destination`.
3. `capture.preview` with the bounded page snapshot (`offline: false` allows the DOI complement).
4. Show the candidate and `pdf_candidates`; on approval `capture.commit` with
   `Create`/`Skip` and an optional `pdf` index; on cancel `capture.cancel`. On `conflict`
   preview again.
5. Optionally poll `capture.pdf_job` / offer `capture.retry_pdf`.

Use one-shot `sendNativeMessage` or a port; disconnect ports when done (an idle port is closed
after 60 s). Keep previews short-lived (≤ 300 s, #33) and commit promptly.

## Known limits

- One library per user installation (the one in `config.json`).
- The same-UID limitation of the security ADR applies: a local process of the user can run the
  host with an allowed origin argument or read the 0600 credential.
- No automatic hook re-runs `refresh` yet; #34 must call it on every app launch. Until then a
  moved `.app` needs a manual `papers-native-host refresh` (Linux: re-run the installer).
- Not verified: macOS (paths, `.app` launch, quarantine/signing #31), branded Google Chrome and
  Chrome Web Store/AMO-signed builds (only Playwright's Chromium and Firefox ESR with a
  sideloaded unsigned test add-on), Windows (registry registration is not implemented).
