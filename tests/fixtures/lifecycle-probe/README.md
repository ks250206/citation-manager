# Test-only lifecycle probe

No product crate, database, authentication grant, user profile, library API, REST
server, installer or launchd job is implemented here. `probe.rs` is a standalone
Rust fixture with a fixed sibling-helper path, scoped nonce, private socket and
2 s idle exit. It admits only exact nonce pings and acknowledgements of the
returned process IDs, with a 4096-byte frame limit. It cannot execute arbitrary
commands or read arbitrary paths. Same-UID adversarial-process isolation is not
claimed. Connected sockets have a 15 s read timeout; the browser fixture pings
at 400 ms intervals. There are no background jobs to test.

`background.js` is the source of a local fixture extension with only
`nativeMessaging`, no network/page/host permissions. Two ports start concurrently;
the received host/daemon PIDs are acknowledged, a host death triggers reconnection,
ports disconnect at 7 s, and a new port opens at 11 s after daemon idle exit.
The browser controller must observe actual callback-derived acknowledgements,
not merely host framing or direct spawn results, before recording browser success.

Current machine browser trials failed under the additional isolation sandbox,
without producing such acknowledgements. The browser runner now refuses the
interactive user's OS account: it requires a disposable `cm34test-*` account/VM.
Do not disable the browser sandbox or extension-signature protections to make it
pass. Firefox's test-only directory-provider override is copied in concept from
Mozilla's pinned native-messaging test harness and remains **unverified here**.
It would prove process mechanics via real Firefox connectNative, not default macOS
installer registration. Actual browser EOF/quit/lazy-start tests remain required.

The separate `probe_harness.py` checks direct spawn, concurrent one-instance startup,
EOF, host death, active-client retention, idle exit/restart and invalid frames.
Its reports deliberately say `mode=direct-spawn-only`, `browser_tested=false`.
They are supplemental evidence and cannot open the #23/#25 browser gate.
