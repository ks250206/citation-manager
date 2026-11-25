# Installing Citation Manager on macOS

This guide covers installing, updating and removing the macOS app and everything around it:
the browser extension and its Native Messaging host, the `papers` CLI, MCP clients, the local
REST API and backups. The bundle layout and the rules the commands below rely on are in the
[app layout ADR](ADR/app-layout.md); recovery from problems is in [RECOVERY](RECOVERY.md);
licenses in [LICENSES](LICENSES.md).

> **Signing status.** Builds are currently **not signed with a Developer ID and not
> notarized** (no Developer ID certificate is available yet; see
> [LICENSES](LICENSES.md#signing-and-notarization) and #31). macOS will refuse to open a
> downloaded copy until you explicitly allow it (step 3). Only do that for a build you made
> yourself or obtained from a source you trust.

Requirements: macOS 13 Ventura or later on Apple Silicon (the published build is arm64; the
bundled PDFium is universal). About 200 MB of disk for the app.

Paths used below:

```sh
APP="/Applications/Citation Manager.app"
BIN="$APP/Contents/MacOS"
LIB="$HOME/Library/Application Support/Citation Manager/library"   # default library
```

## 1. Install the app

1. Open `Citation-Manager-<version>.dmg`.
2. Drag **Citation Manager** onto the **Applications** link in the window, then eject the
   disk image.
3. First start of an unsigned build: in Finder, Control-click the app → **Open** → **Open**
   (or System Settings → Privacy & Security → **Open Anyway**). For a build you produced
   yourself you can instead remove the download quarantine:
   `xattr -dr com.apple.quarantine "$APP"`.

Do not run the app from the mounted disk image or straight from Downloads without moving it:
macOS then runs a temporary, read-only copy ("App Translocation"), and browser registration
refuses that location.

On first start the app creates its library in
`~/Library/Application Support/Citation Manager/library` (owner-only permissions). To use a
different library, start it with an explicit path, e.g.
`open -a "Citation Manager" --args --library "/Volumes/Data/My Library"` (keep libraries out
of Documents/Desktop/Downloads, which macOS guards with privacy prompts).

Nothing runs in the background permanently. The app (and the CLI, MCP server and browser
host) start the library service `papersd` from inside the bundle when needed; it exits about
30 seconds after the last client disconnected and no job runs. There is no login item and no
launchd agent to remove.

## 2. The `papers` CLI on PATH

Link (do not copy) the bundled executables, so they keep using the bundle's `papersd`:

```sh
mkdir -p ~/.local/bin
ln -sf "$BIN/papers" ~/.local/bin/papers
ln -sf "$BIN/papers-mcp" ~/.local/bin/papers-mcp
# zsh: make sure ~/.local/bin is on PATH (e.g. in ~/.zprofile)
export PATH="$HOME/.local/bin:$PATH"
export PAPERS_LIBRARY="$LIB"      # or pass --library each time
papers server status               # {"running": false} until something uses the library
papers list
papers completions zsh > ~/.zfunc/_papers   # optional
```

The links survive app updates in `/Applications`. If you move the app, re-create them.

## 3. Browser capture (Chrome / Firefox)

Capture uses the browser extension plus the Native Messaging host
`papers-native-host`, which the browser starts on demand; the desktop app does not need to
be running.

1. Install the extension for your browser. The store-published extension IDs are listed
   in `"$APP/Contents/Resources/native-messaging/extension-ids.json"` under `release`.
   **Until the extension is published** (that list is empty) only the development flow of
   [browser-extension/README](../browser-extension/README.md) is available; it uses IDs that
   anyone can claim, so never register them on a machine used for real captures.
2. Register the host for that extension and pair it with the library (pairing starts
   `papersd` once and stores a limited capture-only credential):

   ```sh
   "$BIN/papers-native-host" register --library "$LIB" \
     --chrome-extension-id <chrome id> --firefox-extension-id <firefox id> --pair
   "$BIN/papers-native-host" status      # exit 0 and "healthy": true
   ```

   Manifests are written for the browsers whose profile directory exists
   (`~/Library/Application Support/Google/Chrome/NativeMessagingHosts/`,
   `.../Chromium/NativeMessagingHosts/`, `~/Library/Application Support/Mozilla/NativeMessagingHosts/`);
   `--browser chrome|chromium|firefox[=DIR]` selects explicitly.
3. Restart the browser and save a page from the extension's toolbar button.

The manifests contain the absolute path of the host inside the app. Opening the app re-points
them automatically after the app was moved or updated (`papers-native-host refresh`, which you
can also run yourself). The pairing credential is valid for 30 days; when the extension
reports "not paired", run `"$BIN/papers-native-host" pair`.

## 4. MCP clients

`papers-mcp` is a stdio MCP server, read-only by default. Issue its credential once (as the
library owner), then point the client at the bundled executable:

```sh
"$BIN/papers-mcp" grant --library "$LIB"            # read-only, valid 90 days
"$BIN/papers-mcp" grant --library "$LIB" --write    # optional: writes, valid 8 hours
```

Client configuration (the usual `mcpServers` JSON; use absolute paths, clients do not read
your shell PATH):

```json
{
  "mcpServers": {
    "citation-manager": {
      "command": "/Applications/Citation Manager.app/Contents/MacOS/papers-mcp",
      "args": ["serve", "--library", "/Users/<you>/Library/Application Support/Citation Manager/library"]
    }
  }
}
```

`papers mcp` (the CLI wrapper) is equivalent. Revoke with `"$BIN/papers-mcp" revoke --library "$LIB"`
and list grants with `grants`. See the [MCP ADR](ADR/mcp-server.md) for the tool list.

## 5. Local REST API (off by default)

The HTTP API is disabled until you enable it; it only listens on 127.0.0.1/::1 and every
request needs a token.

```sh
"$BIN/papersd" api set --library "$LIB" --enable --port 39271
"$BIN/papersd" token issue --library "$LIB" --label "my script" --kind api \
    --scope papers:read --scope files:read        # prints the token once (terminal only)
curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:39271/api/v1/papers
"$BIN/papersd" api set --library "$LIB" --disable
```

The same settings are in the app under **API & integrations**. While the API is enabled,
`papersd` stays running. Contract: [REST ADR](ADR/rest-api.md), [OpenAPI](openapi.json).

## 6. Backup and restore

Everything that belongs to a library is inside its directory (`$LIB`): the SQLite database,
the immutable attachment store, and `backups/` with the automatic `initial.sqlite` and a
`pre-migration-*.sqlite` snapshot before every schema upgrade. Settings default to
`$LIB/ui-settings.json`.

Back up a library while nothing uses it:

```sh
osascript -e 'quit app "Citation Manager"'
until ! pgrep -f "$BIN/papersd" >/dev/null; do sleep 5; done   # idles out ~30 s after the last client
ditto "$LIB" "/Volumes/Backup/citation-manager-$(date +%Y%m%d)"
```

Restore into a library location that is not in use (keep the damaged copy for diagnosis):

```sh
mv "$LIB" "$LIB.damaged-$(date +%s)"
ditto "/Volumes/Backup/citation-manager-20261125" "$LIB"
open -a "Citation Manager"
```

Time Machine also covers `~/Library/Application Support`, but a copy taken while `papersd`
writes can need SQLite's recovery on open. Not included in a library copy: API/MCP token
secrets (Keychain items of service `citation-manager`, or the fallback files in
`~/Library/Application Support/citation-manager/secrets/`) and the browser pairing
(`~/Library/Application Support/Citation Manager/native-host/`); issue new ones after a
restore. A verified portable backup/restore exists in the storage layer
([attachments-backup ADR](ADR/attachments-backup.md)) but has no app or CLI command yet.
See [RECOVERY](RECOVERY.md) for corrupted libraries and failed upgrades.

## 7. Update

1. Quit the app and wait until no `papersd` runs (`pgrep -fl papersd` prints nothing; an old
   `papersd` would otherwise keep serving the library until it idles out).
2. Drag the new app from the new disk image onto **Applications** and choose **Replace**.
3. Open it once. Schema upgrades run with an automatic pre-migration snapshot; browser
   manifests are re-pointed; CLI links in `~/.local/bin` keep working.

## 8. Uninstall

```sh
"$BIN/papers-native-host" unregister            # removes browser manifests, revokes the pairing
"$BIN/papers-mcp" revoke --library "$LIB"       # if you issued an MCP grant
"$BIN/papersd" api set --library "$LIB" --disable
osascript -e 'quit app "Citation Manager"'
rm -f ~/.local/bin/papers ~/.local/bin/papers-mcp
mv "$APP" ~/.Trash/
```

Your data stays until you delete it yourself:

- the library: `~/Library/Application Support/Citation Manager/library` (and any library you
  created elsewhere) — **this is your data; back it up first**;
- host registration: `~/Library/Application Support/Citation Manager/native-host/`;
- token secrets: Keychain items with service `citation-manager` (Keychain Access) and
  `~/Library/Application Support/citation-manager/secrets/`;
- the socket directory `$TMPDIR/citation-manager-<uid>/` disappears on its own.
