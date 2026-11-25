# Recovery guide (macOS)

What to do when the app, the library service, PDF support, browser capture or a library is
not in a good state. Installation, backups and uninstalling are in [INSTALL](INSTALL.md).
Never edit `library.sqlite` or the `attachments/` tree by hand; always work on copies.

```sh
APP="/Applications/Citation Manager.app"; BIN="$APP/Contents/MacOS"
LIB="$HOME/Library/Application Support/Citation Manager/library"
```

## The app does not open

| Symptom | Cause | Action |
|---|---|---|
| "cannot be opened because the developer cannot be verified" / "Apple could not verify" | build is not notarized (current builds) | [INSTALL step 1.3](INSTALL.md#1-install-the-app) |
| "is damaged and can't be opened" | the bundle was modified after signing, or copied without its extended attributes | reinstall from the disk image (copy with Finder or `ditto`, not `cp -R`); `codesign --verify --deep --strict "$APP"` shows what changed |
| nothing happens, `Cannot prepare library service` in Console | the library path is not absolute/accessible, or `HOME` is unusable | start with `open -a "Citation Manager" --args --library /absolute/path` and check its permissions (0700, owned by you) |
| the window opens but the library cannot be reached | `papersd` could not start or was refused | see the next section |

## The library service (`papersd`)

`papersd` runs from inside the bundle, one per library, and exits by itself when idle.

- **State:** `"$BIN/papers" --library "$LIB" --no-start server status` (`running`, `pid`,
  `daemon_version`, `connections`, `active_jobs`). Every connection, including this probe,
  counts as activity, so do not poll it in a tight loop while waiting for the idle exit.
- **Stuck or from an older app version:** quit all clients (app, `papers`, MCP clients,
  close the browser), then wait about 30 s. If it still runs:
  `pkill -TERM -f "$BIN/papersd"` (`papers server stop` is not available on macOS yet). A
  terminated daemon recovers interrupted jobs on its next start; SQLite rolls back unfinished
  transactions.
- **"another daemon already serves this library" (exit 75) but clients cannot connect:** a
  daemon started from an environment with a different `TMPDIR` (e.g. an SSH session) owns
  the library; its socket is in another directory. Stop that process (`pgrep -fl papersd`)
  or use the same `--runtime-dir` everywhere.
- **"socket path too long":** pass a short `--runtime-dir` (owner-only directory) to every
  client, or unset an unusually long `TMPDIR`.
- **Permission errors on the library or runtime directory:** both must be owned by you and
  not group/other-accessible: `chmod 700 "$LIB"`.

## PDF viewing, text extraction or rendering unavailable

PDF support uses the bundled `Contents/Frameworks/libpdfium.dylib` and loads it only when its
SHA-256 is one the executables were built to trust: the pinned upstream build, or the copy the
release job signed (recorded as `bundled_sha256` in `Contents/Resources/pdfium-runtime.json`).
A modified, re-signed or missing dylib disables PDF features (papers and attachments are
unaffected).

```sh
shasum -a 256 "$APP/Contents/Frameworks/libpdfium.dylib"
python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["bundled_sha256"])' \
  "$APP/Contents/Resources/pdfium-runtime.json"
```

If they differ, reinstall the app (never re-sign the bundle yourself). `papers fulltext <paper>`
shows the extraction state of an attachment. Image-only PDFs report "no extractable text";
OCR is not provided.

## Browser capture fails

| Extension message | Action |
|---|---|
| host not installed / not found | open the app once (re-points the manifests after a move or update) or run `"$BIN/papers-native-host" refresh`; `status` must print `"healthy": true` |
| "… is an App Translocation copy" or "… read-only volume" from `register`/`refresh` | move the app into `/Applications` (not run from the disk image or Downloads), open it, register again |
| not paired / unauthorized | `"$BIN/papers-native-host" pair` (the pairing lasts 30 days) |
| `daemon_unavailable` | see the library service section |

`"$BIN/papers-native-host" status` exits non-zero and names the problem (stale manifest,
missing browser, pending revocation). `unregister` followed by `register … --pair` rebuilds
the registration from scratch.

## A library is damaged or an upgrade failed

1. Quit every client and make sure no `papersd` runs (see above).
2. Copy the whole library directory somewhere safe before trying anything:
   `ditto "$LIB" "$LIB.investigate-$(date +%s)"`.
3. Restore, in order of preference:
   - a backup you made ([INSTALL §6](INSTALL.md#6-backup-and-restore)) into the library path;
   - after a failed schema upgrade, the automatic snapshot taken right before it:
     `ls -t "$LIB/backups/pre-migration-"*.sqlite` — a standalone database of the previous
     schema. Replace `library.sqlite` (and delete `library.sqlite-wal`/`-shm`) with a copy of
     it **in a copy of the library**, then open that copy with
     `open -a "Citation Manager" --args --library "/path/to/copy"`. An older app version is
     needed to open an older schema; the current app upgrades it again (with a new snapshot);
   - `backups/initial.sqlite`: the empty schema created with the library (last resort; papers
     are lost, attachments stay in `attachments/` and can be re-attached).
4. Attachment files are content-addressed (`attachments/ab/cd/<sha256>.pdf`) and never
   modified; a library whose database is lost still has its PDFs.

The full-text index is derived data: it is rebuilt from the attached PDFs and never affects
bibliographic records.

## Tokens and grants

Lost or leaked API/MCP token: revoke it (`"$BIN/papersd" token list|revoke --library "$LIB"`,
`"$BIN/papers-mcp" revoke`, or **API & integrations** in the app) and issue a new one.
Every token expires (read 90 days, write 30 days by default). The browser pairing is renewed
with `papers-native-host pair`.

## Collecting diagnostics

`papers` prints a JSON envelope with an error code and exit status; `papers-native-host`
diagnostics appear in the browser's log (Chrome: its stderr; Firefox: Browser Console);
`papersd` writes a content-free audit log at `$LIB/daemon/audit.jsonl`. None of them contain
page content or secrets, so they can be attached to an issue after review.
