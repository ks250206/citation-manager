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
| .app resource配置/遅延起動/寿命 #34 | implementing agent + macOS実機操作可能 agent。fallback Ryoji Kanno | hosted macOS 15 runner で検証済み（[APP_LAYOUT_POC](APP_LAYOUT_POC.md): 配置/otool/PDFium描画・抽出/遅延起動/idle終了/移動後GUI・再登録/Chrome for Testing・Firefox probe）。**実機は未実施**（Finder導入・quarantine・同時起動・sleep/logout・TCC） |
| 認可境界 #25/#26 | implementing agent が自動セキュリティ回帰、ユーザー同意UIは実機可能 agent / Ryoji Kanno | #25: Linux 自動回帰 passed（[ISSUE_25](ISSUE_25.md)）。#26: Linux 自動回帰と Xvfb GUI 操作 passed（[ISSUE_26](ISSUE_26.md)）。macOS peer/.app と Keychain は**未実施** |
| ローカル REST API #27 | implementing agent が Linux 自動回帰（実 papersd、ephemeral loopback port）と curl smoke | Linux 自動回帰と curl smoke passed（[ISSUE_27](ISSUE_27.md)）。macOS .app/launchd (#34)、PDFium 描画成功・DOI 実ネットワーク解決、GUI hint の実機操作は**未実施** |
| 全文抽出/検索 #18 | implementing agent が Linux 自動回帰と pin 済み Linux PDFium 実抽出 | Linux 自動回帰と実 PDFium 抽出→検索 passed（[ISSUE_18](ISSUE_18.md)）。macOS PDFium/.app 同梱 (#34)、GUI 検索 (#21) は**未実施**。1万件/1000 PDF/2万頁負荷は #30 で Linux コンテナと GitHub macOS VM runner で実測（[ISSUE_30](ISSUE_30.md)）、Apple Silicon 16 GB 実機は**未実施** |
| Chrome/Firefox保存 #23/#24 | implementing agent がfixture、自動化可能な実ブラウザ agent。fallback Ryoji Kanno | #23 host: Linux の Chromium（Playwright 同梱）と Firefox ESR 153 で開発用 test 拡張 → host → 実 papersd の preview/commit、host 終了・ブラウザ終了・GUI 非起動のまま保存と PDF job 終端を確認（[ISSUE_23](ISSUE_23.md)）。macOS/Windows/正式版 Chrome・署名済み拡張は**未実施**。#24 の実拡張 UI と重複/再試行の手動記録は**未実施**。#22 の daemon 側 capture/PDF job は Linux 自動試験済み（[ISSUE_22](ISSUE_22.md)） |
| MCP stdio tools #29 | implementing agent が公式SDK/clientから自動試験 | **未実施**。入口は Unavailable。read-only/write許可・出典ページ・stdout純度・transport停止を検証 |
| Developer ID / notarization / DMG #31 | Ryoji Kanno が Developer ID と Apple 公証権限を用意し、権限を与えた配布担当 agent が実行 | **blocker**: `security find-identity -v -p codesigning` → `0 valid identities found`。Developer ID署名・notarizationは未検証。秘密は保存/Issue公開しない。DMG作成・notices同梱・ad-hoc hardened runtime は hosted runner で検証、署名手順は script 化し secrets 無しでは `UNVERIFIED: no Developer ID`（[ISSUE_31](ISSUE_31.md)） |
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
cycles. After normally merging main `5f0fb1d`, a fresh binary retested dirty guards, attachment reopening, PDF metadata preview and cancellation; exact phase hashes and integration checks are retained. No command/keymap native handlers were connected. D&D on the PDF files / folder screen subsequently passed both intents in the assisted
run below; duplicate Details/PDF zones remain untested. Offline DOI, simultaneous DOI/PDF
cancellation and failed page-query retry remain unverified. Keep PR45 Draft and #15 open. #34 remains HELD; #23/#25 gates remain
closed. Root owns independent final review, CI and any merge.

The [DOI/PDF ownership follow-up](issue-15/doi-pdf-ownership.md) supersedes the previous
incomplete concurrency guard explanation: the reachable review-tab race was fixed,
with actual pending-lookup cancellation/retained-PDF commit and disabled DOI controls
during PDF inspection observed. Already-queued reply rejection/token isolation is
automated state evidence; native late-arrival/simultaneous-operation proof is not claimed.
Final integrated local checks: workspace227, native desktop43, Python28; #53 main
integration preserves all 636 pins and migrations. D&D was subsequently exercised on the dedicated PDF screen; offline DOI/failed-page retry
remain incomplete; Draft/#15 and held #34 gates are unchanged.

[Assisted native Finder D&D](issue-15/native-dnd.md) subsequently passed both explicit
Attach and NEW intents: user gestures, root CUA review/save/relaunch/title correction,
and read-only DB/hash checks are separately identified. Two papers/three attachments
remain correct with original bytes intact. This supersedes D&D's earlier unverified
status only. Offline DOI / real failed-page / native queued-late-arrival evidence remains
unverified. No runtime changes or full-suite rerun; final acceptance review/CI pending,
PR45 Draft and #15 open, #34 HELD and #23/#25 gates unchanged.


[Latest-runtime bulk retest](issue-15/native-bulk.md) now covers the native 1000-record
import/classify/search/re-export sequence on the final unchanged runtime in a fresh
private library. Ten explicit 100-row approvals committed all 1000 records; native
navigation/scrolling responded during preview work. All/selected/collection RIS outputs
reparsed to 1000/1/1 exact normalized records. This is debug-build functional evidence,
not release latency/p95/RSS proof. Root accepts the offline PDF filename/edit criterion
by the documented network-free path plus actual fallback/editing; a network-disabled
GUI experiment, offline DOI, failed-page retry and native queued-late-arrival remain
unverified. PR45 stays Draft and #15 open pending independent acceptance review/current
CI. No #34 or #23/#25 gate is opened by this evidence.

## #30 横断回帰・代表負荷（2026-11-25）

詳細・数値・コマンドと exit code は [ISSUE_30](ISSUE_30.md)、生 JSON は [ISSUE_30/](ISSUE_30/)。
実行主体はすべて implementing agent。「実測」は記載の環境での値であり、DESIGN §16.1 の
Apple Silicon 16 GB release 実機の値ではない。

| 対象 | 環境 | 証跡 | 状態 |
|---|---|---|---|
| 強制終了→DB/job 復旧、migration 履歴不整合、backup 復元、索引破損→再構築、孤立添付 | Linux 自動（実 papersd） | `tests/integration/daemon/recovery.rs`（`daemon_recovery`）+ 既存 lifecycle/jobs/CRASH_RECOVERY | passed |
| 2段組/数式/表/縦書き/回転 PDF | Linux 自動（pin 済み PDFium 8125） | `crates/pdf/tests/layouts_runtime.rs`、`fixtures/library/layouts.pdf` | passed（縦書きは非埋め込み UniJIS-UCS2-V のみ） |
| スキャン/パスワード/破損 PDF | Linux 自動（同上） | 既存 `extract_runtime`、`tests/integration/daemon/fulltext.rs` | passed（既存） |
| 悪意ある HTML/JSON/IPC frame・SSRF・認可迂回 | Linux 自動 | `crates/metadata/tests/mutation_lite.rs`、`tests/integration/daemon/hostile.rs` + 既存 auth/rest/api_tokens/fetch_policy | passed |
| 1万件/1000 PDF/2万頁 headless 負荷（起動・切替・検索・日本語再現率・RSS/CPU/I/O） | Linux コンテナ / GitHub macOS VM runner | `scripts/benchmarks/representative.py`、`.github/workflows/benchmark.yml` | 実測（16 GB 実機は**未実施**） |
| GUI idle / PDF 閲覧 RSS | Linux Xvfb（ソフトウェア描画） | 同上 `--gui`、`ISSUE_30/linux/gui.json` とスクリーンショット | 実測（macOS GUI は**未実施**） |
| フレーム落ち（一覧スクロール） | なし | — | **未測定**（フレーム時間の計測手段なし） |
| PDFium 以外での注釈表示 | Linux CI（pypdf + poppler） | 既存 `scripts/pdfium/validate_export.py`（ci.yml linux-native）、[pdf-export ADR](../ADR/pdf-export.md) | ISSUE_30 に再実行結果 |
| host/daemon 寿命、lease/retry_wait 復帰 | Linux 自動 | 既存 `daemon_lifecycle`、`crates/native-host/tests/host_process.rs` | passed（既存、本 PR の全体実行で再確認） |

### #85 PDF 閲覧時のメモリ削減（2026-11-25）

数値・手順は [ISSUE_30 の #85 節](ISSUE_30.md#85-pdf-閲覧時のメモリ削減)。

| 対象 | 環境 | 証跡 | 状態 |
|---|---|---|---|
| binary frame（blob）の符号化・上限・不正 frame（長さ不一致/未参照/二重参照/過大宣言/切断） | Linux 自動 | `crates/protocol/src/ipc/frame.rs` の unit test | passed |
| protocol 2: v1 client は平文 JSON の `VersionMismatch{supported: 2}`、認証前後の不正 binary frame は実行されない | Linux 自動（実 IPC） | `tests/integration/daemon/ipc.rs`（`daemon_ipc`）、既存 `daemon_hostile` | passed |
| 実 PDFium の page render が binary frame の生 RGBA で届く | Linux 自動（pin 済み PDFium） | `tests/integration/daemon/fulltext_runtime.rs`（ci.yml linux-native） | passed |
| REST/MCP の JSON 契約（base64 の page render を含む）不変 | Linux 自動 | 既存 `daemon_rest`（OpenAPI 契約）、MCP tests | passed |
| GPU 転送後の CPU 画素解放と再表示（scroll back・再描画・evict 後の再要求・回転/文書切替） | Linux 自動 + Xvfb 操作 | `crates/desktop/tests/pdf_viewer.rs`、スクリーンショット | passed |
| glibc mmap 閾値の再 exec（既存設定の尊重、無限 re-exec しない） | Linux 自動 | `crates/daemon/src/malloc_tuning.rs` の unit test | passed |
| PDF 1 本閲覧時の総 RSS | Linux Xvfb | `representative.py --gui`（ISSUE_30/linux-m62/） | 実測（macOS GUI は**未測定**） |
| papersd の描画後 RSS / footprint（headless） | Linux コンテナ / GitHub macOS VM runner | `representative_ipc renders`（ISSUE_30/linux-m62/、ISSUE_30/macos-m62/） | 実測 |
