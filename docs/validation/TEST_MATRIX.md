# #7 検証分担と証跡

以下は#7時点の履歴。現在のSQLite実装と2本のmigrationは [#8検証](ISSUE_8.md)、
PDFium実ロード/継承box/独立renderer検証は [#11検証](PDFIUM_POC.md) を参照。
下記の0件registry・PDFium未選択は現行の状態を示すものではない。

2026-11-06、実装 agent がこの isolated worktree で実行。
環境: macOS 27.2 (26B123), arm64、rustc 1.99.1 (58cf8f9c3 2026-10-13)、
cargo 1.99.1 (5f94df478 2026-08-27)、Xcode 26.6 (17F113)、Python 3.14.0。
base main: `86eb892`。依存確認は [DEPENDENCIES](DEPENDENCIES.md) を参照。

## #7 の自動検証（実装 agent の責任）

| 実コマンド | 結果 / 証跡 |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed、workspace warning なし |
| `cargo test --workspace --locked` | passed: integration/unit 9件 + compile-fail doctest 1件 |
| `cargo build --workspace --locked` | passed、12 library crate。実行可能な app/server ができたという意味ではない |
| `python3 scripts/check_architecture.py` | passed、12 member・許可依存方向・framework/driver 所有・macOS GPUI pin |
| `python3 scripts/check_migrations.py --base origin/main` | passed、0件 registry と base 保存。実 schema は未実装 |
| `python3 -m unittest discover -s scripts -p 'test_*.py' -v` | passed、13件。逆依存・framework混入・driver所有・GPUI固定rev/optional/macOS条件、migration衝突/改変/削除・registry改変・symlink・subdirectory・無効baseを拒否 |
| `xcodebuild -downloadComponent MetalToolchain` | passed、Metal Toolchain 17F109 を取得 |
| `cargo check -p cm-desktop --features native-gpui --locked` | passed、gpui/gpui_platform/gpui_macos/gpui_apple 固定 rev、Metal shader compile を含む |
| `cargo clippy -p cm-desktop --all-targets --features native-gpui --locked -- -D warnings` | passed |
| `cargo build -p cm-desktop --features native-gpui --locked` | passed、macOS backend を実際に compile。window/event loop は呼ばない |
| `cargo check -p cm-pdf --features bindings-check --locked` | passed、pdfium-render 0.9.5 / pdfium_8125 の Rust bindings のみ |
| `git diff --check`（stage後は `git diff --cached --check`） | passed |

通常 lane は純 Rust の契約検証と SQLite driver probe（bundled C build を含む）。
GPUI と PDFium binding は別 feature lane。native lane 成功を UI/PDF runtime の成功と混同しない。
CI は macos-15 で同じ fmt/clippy/test/build と script、別 job で native dependency lane を実行する。
GitHub runner の OS/Xcode 差による失敗は PR 上で実結果を確認する。ローカル passed は CI passed の代用ではない。
初回 CI の native lane は runner の Xcode 16.4 で未対応の
`-downloadComponent` を無条件に使ったため失敗した。Metal compiler の実行可否を
先に確認し、利用できないときだけ追加取得するよう修正した。修正版の CI 結果は PR 上で確認する。
供給された DESIGN.md の Markdown hard break は原文維持のため `.gitattributes` で EOL 空白検査から除外する。

テストの内容:

- domain: page index/display の 0・1・最大値・overflow・負値、deserialize 時の拒否、PDF label `iv` の保持、
  Paper/Attachment と外部 ID の roundtrip、`source=ai`、位置未解決 quote の保存、storage key の traversal 拒否。
  Unresolved の再出力に位置を持たず、未知の page フィールドでも Resolved へ昇格しないことを確認する。
- core: transport/framework なしで Repository を差替え、preview/commit の別 DTO と明示的 skip を spy で確認。
  compile-fail test は raw ImportRequest を commit に渡せないことを保証する。
  spy は production service の認可/transaction/重複処理が実装済みである証拠ではない。
- storage: メモリ内の独立 probe で bundled SQLite の FTS5 と backup API を確認。製品 DB は作らない。
- mcp: stdio 入口が明確な Unavailable エラーを返すことを確認。handshake/tools は出力しない。

初回失敗も記録する: Metal component 導入前の native check は
`cannot execute tool 'metal' due to missing Metal Toolchain` で失敗。
上記 component 取得後、同一 GPUI rev の check/clippy/build を再実行して passed。
最初の Rust test は test 内の `StorageKey::try_from("...".into())` の型推論で失敗し、
明示的 String に修正後、全 suite を再実行して passed。未修正の test failure はない。

## 後続の実機 / 権限が必要な検証

| 対象 / Issue | 実行主体 | 現状・blocker・必要な証跡 |
|---|---|---|
| GPUI window / 保存→再起動→閲覧 #10 | implementing agent が自動検証、UI操作可能な agent が実機検証。利用不可なら Ryoji Kanno (ks250206) | #10で実 window と#8保存を検証済み。[手順・画面・結果](ISSUE_10.md)。日本語IME候補選択は未検証。#7当時の未実施記録は上記の履歴 |
| PDFium runtime/ABI/文字位置/注釈 roundtrip #11 | implementing agent が fixture 自動検証、UI可能 agent。fallback Ryoji Kanno | **未実施**。dylib 未選択/未同梱。ABI/load path、corpus、Unicode/回転/複数行、出力再読込を証拠化 |
| .app resource配置/遅延起動/寿命 #34 | implementing agent + macOS実機操作可能 agent。fallback Ryoji Kanno | **未実施**。.app 未作成。bundle移動/GUIなし/同時起動/idleとjob中の寿命/PDFium pathを記録 |
| 認可境界 #25/#26 | implementing agent が自動セキュリティ回帰、ユーザー同意UIは実機可能 agent / Ryoji Kanno | **未実施**。IPC/credential/grant/authorizer未実装。ADR列挙の拒否・失効・偽装・MCP別write許可を検証 |
| Chrome/Firefox保存 #23/#24 | implementing agent がfixture、自動化可能な実ブラウザ agent。fallback Ryoji Kanno | **未実施**。拡張/host未実装。両ブラウザでGUI閉鎖時の保存/重複/失敗再試行を記録 |
| MCP stdio tools #29 | implementing agent が公式SDK/clientから自動試験 | **未実施**。入口は Unavailable。read-only/write許可・出典ページ・stdout純度・transport停止を検証 |
| Developer ID / notarization / DMG #31 | Ryoji Kanno が Developer ID と Apple 公証権限を用意し、権限を与えた配布担当 agent が実行 | **blocker**: `security find-identity -v -p codesigning` → `0 valid identities found`。Developer ID署名・notarizationは未検証。秘密は保存/Issue公開しない |
| 独立異モデル review | root が非対話/read-only の別モデルを手配 | **実施**。Opus 5.5/mediumが `e927a5f` を確認しP0〜P2なし、P3のtest/doc指摘3件。本変更で対応し、修正後headのreview結果と証跡はPRコメントに記録する。自己reviewで代替しない |

UI/browser の未実施は自動 contract test で充足した扱いにしない。fallback ユーザーがまだ実行していない項目を
passed と記さない。Developer ID 不在は将来 #31 の blocker として維持し、#7 の未署名 compile を妨げない。

## #15 library workbench (2026-11-07, draft)

[ISSUE_15](ISSUE_15.md) / [exact command logs and hashes](issue-15/checks.json):
workspace fmt/clippy/test/build (164 tests plus one subsequent targeted attachment test),
architecture, immutable migration base fd50af1 and Python 19 validators passed.
Native-feature check/clippy/service-state tests (53) and executable link passed; a current
desktop-only 13-test pass follows the final UI label/progress-coalescing changes.
Real fixed-hash PDFium runtime/metadata tests (8) and runtime clippy passed. All 636 locked
package identities/checksums remain unchanged. These are headless/compile/ABI results.

**Native GUI works; acceptance remains incomplete.** The historical cgWindowNotFound
failures above are superseded by the [awake run](issue-15/native-awake.md): actual ten-batch
1000-row import, classification/search/export, pickers/folder/partial retry, settings
quit/relaunch and attachment host actions. That report distinguishes earlier-binary proof
from affected-control retests. The [fresh continuation](issue-15/native-continuation.md)
adds exact-executable evidence for collection rename/move/delete, both oversize limits,
dirty selection/Trash guards, unrelated Attach/Get preservation and six preview/Resume
cycles. After normally merging main `5f0fb1d`, a fresh binary retested dirty guards, attachment reopening, PDF metadata preview and cancellation; exact phase hashes and integration checks are retained. No command/keymap native handlers were connected. Genuine D&D, offline DOI, simultaneous DOI/PDF cancellation and failed page-query
retry remain unverified. Keep PR45 Draft and #15 open. #34 remains HELD; #23/#25 gates remain
closed. Root owns independent final review, CI and any merge.

The [DOI/PDF ownership follow-up](issue-15/doi-pdf-ownership.md) supersedes the previous
incomplete concurrency guard explanation: the reachable review-tab race was fixed,
with actual pending-lookup cancellation/retained-PDF commit and disabled DOI controls
during PDF inspection observed. Already-queued reply rejection/token isolation is
automated state evidence; native late-arrival/simultaneous-operation proof is not claimed.
Final integrated local checks: workspace227, native desktop43, Python28; #53 main
integration preserves all 636 pins and migrations. D&D/offline DOI/failed-page retry
remain incomplete; Draft/#15 and held #34 gates are unchanged.
