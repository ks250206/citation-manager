# #30 障害復旧・セキュリティ回帰・1万件性能測定

2026-11-25、implementing agent が worktree `claude/issue-30-reliability` で実施。
既存の局所テスト（#8/#9/#18/#22/#25–#29、[CRASH_RECOVERY](CRASH_RECOVERY.md)、
[BACKEND_BENCHMARK](BACKEND_BENCHMARK.md)）は再利用し、欠けていた横断シナリオ・敵対入力・
代表負荷だけを追加した。生データは [ISSUE_30/](ISSUE_30/)、検証分担表は
[TEST_MATRIX](TEST_MATRIX.md#30-横断回帰代表負荷2026-11-25)。

**最重要の制約:** DESIGN §16.1 の測定機（Apple Silicon、16 GB RAM、release）は利用できない。
以下の数値は (a) Linux コンテナと (b) GitHub Actions の macOS arm64 **仮想** runner（7 GB）の
実測であり、16 GB 実機の値ではない。未測定の項目は「未測定」と書き、達成扱いしない。

## 受け入れ条件と証跡

| 受け入れ条件 | 状態 | 証跡 |
|---|---|---|
| 強制終了後の DB/job 復旧、migration failure、バックアップ復元、索引破損再構築、孤立添付検出の縦断検証 | 達成（Linux 自動） | `tests/integration/daemon/recovery.rs`（新規 3 シナリオ、実 `papersd`）＋既存 `daemon_lifecycle`/`daemon_jobs`/`crash_recovery`/`storage recovery` |
| 2段組/数式/表/縦書き/回転/スキャン/パスワード PDF、悪意ある HTML/JSON/SSRF/API 権限の回帰 | 達成（Linux 自動、縦書きは条件付き） | `crates/pdf/tests/layouts_runtime.rs` + `fixtures/library/layouts.pdf`、既存 `extract_runtime`、`tests/integration/daemon/hostile.rs`、`crates/metadata/tests/mutation_lite.rs`、既存 auth/rest/api_tokens/fetch_policy |
| Apple Silicon 16 GB release で 1万文献/1000 PDF/2万 page、総 RSS/CPU/I/O、warm 起動、検索/collection p95 を公開 | **一部**: 同条件のデータ・release build で Linux と macOS VM runner は実測。16 GB 実機は**未実施** | `scripts/benchmarks/representative.py`、`.github/workflows/benchmark.yml`、[ISSUE_30/linux](ISSUE_30/linux/)、[ISSUE_30/macos](ISSUE_30/macos/) |
| §16 初期目標との差、未達の根拠と調整案、未測定値を達成扱いしない | 達成（下表）。PDF 閲覧 RSS は未達、フレーム落ちは未測定 | 本書「§16.2 との比較」「未達・未測定」 |
| PDF LRU 上限・抽出/取得の並列数の検証、fuzz 導入可否の記録 | 達成。LRU を 192→64 MB に調整 | `crates/desktop/src/pdf_viewer/mod.rs`、`crates/desktop/tests/pdf_viewer.rs`、[ISSUE_30/linux-render-cache](ISSUE_30/linux-render-cache/) |
| 日本語 1/2/3 文字以上の再現率/対象範囲/索引サイズ/p95、PDFium 以外での注釈表示、host/daemon 寿命、認可迂回、lease/retry_wait 復帰、#7 表への紐付け | 達成（下記、既存テストの再実行を含む） | 本書各節、TEST_MATRIX #30 節 |

## 環境

| | Linux（本 worktree） | macOS（GitHub runner） |
|---|---|---|
| ハードウェア | Intel Xeon @ 2.30GHz、4 vCPU、15.7 GiB（16,868,675,584 B） | `VirtualMac2,1`、`Apple M1 (Virtual)`、3 vCPU、`hw.memsize=7516192768`（7 GiB）、`kern.hv_vmm_present=1` |
| OS | Ubuntu 24.04.5 LTS、kernel 6.18.44（Firecracker VM 上のコンテナ、`systemd-detect-virt=docker`） | macOS 15.8.2 (24H232)、image `macos15` 20261118.0550.1 |
| 実行 | `representative.py --gui --out linux-final`、head `ee2a33b`（clean） | workflow run [45596521837](https://github.com/ks250206/citation-manager/actions/runs/45596521837)（workflow_dispatch、head `ee2a33b`）。先行 run 45593261700（head `88cee22`、旧 harness）も成功し同傾向（[macos-run1](ISSUE_30/macos-run1/)） |
| 隔離 | 他の agent と共有ホスト。測定中に自分の build は走らせていないが、他プロセスは制御外 | GitHub 共有 runner、制御外 |

両測定は `ee2a33b` のコードで行った。その後の harness 変更は clippy 指摘の書き換え
（型別名・`Copy` の clone 削除・`prepare` 引数）と集計のみで、測定動作は変わらない。
コミット済み `summary.json` は最終集計関数で生データから再計算した
（`representative.py --resummarize <dir>`。queries の RSS を rusage から sampled `ps` に変更）。

共通: Rust 1.99.1、`--release --locked`、opt-level 3、debug assertions なし。`papersd` は
`--features pdf-metadata`、PDFium は pin 済み chromium/8125（`scripts/pdfium/fetch.py` で
checksum 検証、Linux は `lib/libpdfium.so`、macOS は `.app/Contents/Frameworks/libpdfium.dylib`
の製品配置に staging）。GUI は Linux のみ `--features desktop-bootstrap` の release build を
Xvfb（1400x900x24、GPU なし＝ソフトウェア描画）で起動。

## 方法とデータ生成条件

- **生成器** `scripts/benchmarks/pdfgen.py`（標準ライブラリのみ、seed 30、決定的）:
  1,000 PDF × 20 page = 20,000 page、A4、1 page 34 行（英語風の合成語 17 行 + 日本語 17 行、
  日本語は Identity-H Type0 + ToUnicode、フォント非埋め込み）。生成文字数 29,262,108、
  PDF 合計 38,458,344 B、corpus SHA-256 `886109ea…f2d0`（Linux/macOS で一致）。
  検索語は行内に植え込み（`電池` g%7、`電解質` g%13、`固体電解質` g%97、`電子` g%11、
  `lithium` g%5、`dendrite` g%50、`zirconia` g%997、g は通し page 番号）。背景語彙は
  植え込み語の文字を一切含まないため、生成テキストの**行単位の literal 部分一致**が正解集合
  （oracle.json）になる。
- **文献** `representative_seed.rs`（cm-storage example）: 10,000 件を #47 と同じ専用
  fixture 接続の直接 SQL（1 transaction、FULL/WAL、検索 trigger は発火）で投入。
  10 collection・8 tag に各 2 所属、`needle` は (i+30)%7==0。PDF は 10 件に 1 件の文献へ
  collection/tag が偏らない位置で `ManagedAttachments::import`（copy・再 hash・fsync・
  atomic publish・DB 参照）により添付。
- **索引作成**: 生成 library で `papersd` を起動すると製品の自動抽出（`fulltext.extract`、
  500 添付/run を 2 run）が走り、ジョブ終了後の idle 終了までを計測。
- **IPC 負荷** `representative_ipc.rs`（cm-daemon example）: GUI/CLI と同じ認証付き
  `Session`。warm 起動は `LocalConnector` の `Launch`（GUI/CLI の遅延起動経路）で
  spawn→socket→handshake→`Status`→一覧先頭 100 件までを 11 回（repeat 0 は索引直後なので
  除外し 10 回で集計）。collection 切替（limit 100、GUI と同じ）10 collection × 21 回、
  tag AND 切替、メタデータ literal（hit/no-hit/複合）、検索 DSL（limit 50、GUI と同じ）を
  各 21 回。全 call は IPC round trip（frame encode・認可・service・decode）を含む。
- **再現率**: 各語を `Bounded`（既定）と `AllPages` で、結果 page と continuation window を
  最後まで辿り、page hit を oracle と比較。
- **プロセス計測**: `ps` の RSS/CPU を 0.25–0.5 秒間隔、Linux は `/proc/<pid>/io`、終了時に
  `wait4` rusage。Linux の `ru_maxrss` は exec 前（fork した Python）の最大値を引き継ぐため
  RSS には使わない（sampled `ps` を採用）。macOS の rusage block 数は 0 で I/O は**未測定**。
- 分位点は nearest rank `sorted[ceil(p·N)-1]`。p95 は N=21（切替は 210/168）。

## §16.2 との比較

| 指標（§16.2 目標） | Linux コンテナ | macOS VM runner (7 GB) | 判定 |
|---|---:|---:|---|
| 温かい起動 ~1 s（daemon 遅延起動 + 認証 + 先頭 100 件） | p50 34.7 / p95 38.4 ms | p50 121.0 / p95 209.5 ms | 達成（headless）。GUI window 表示までの時間は**未測定** |
| コレクション切替 p95 < 100 ms | 6.2 ms（p50 4.3） | 3.2 ms（p50 2.6） | 達成 |
| メタデータ検索 p95 < 150 ms（literal 全ケース） | 103.9 ms（hit 13.6 / no-hit 115.0 / 複合 94.4） | 77.2 ms（hit 8.4 / no-hit 86.6 / 複合 77.2） | 達成。no-hit は全件走査で余裕小 |
| 本文全文検索 p95 < 300 ms（索引済、完全な回答） | 34.7 ms | 28.8 ms | 達成 |
| 〃 既定 scope の全語（短語の部分回答を含む） | 57.1 ms | 58.0 ms | 参考 |
| 〃 collection 内 | 67.7 ms | 76.7 ms | 参考（短語は後述の制限あり） |
| 短語 `AllPages` の 1 call（window/結果 page） | p95 121.3 ms | p95 106.7 ms | 1 call は目標内。完全回答は数十 call |
| GUI アイドル RSS < 200 MB | GUI 160.9 MiB（+ papersd 13.3 = 174.2） | — | Linux Xvfb で達成。macOS は**未測定** |
| PDF 1本閲覧時 RSS < 350 MB（総プロセス） | **409.7 MiB**（GUI 320.6 + papersd 89.1）→ #85 後 **263.4 MiB**（GUI 238.2 + papersd 25.2） | — | #30 時点は未達、[#85](#85-pdf-閲覧時のメモリ削減) で達成（Linux Xvfb）。macOS GUI は**未測定** |
| PDF レンダリングキャッシュ上限 64–128 MB | 64 MB に調整（旧 192 MB） | 同コード | 達成（設定） |
| 一覧スクロールでフレーム落ちなし | — | — | **未測定**（フレーム時間を取る手段がない） |
| `papersd` idle RSS（warm 起動直後） | 13.3 MiB | 10.4 MiB | 参考 |
| `papersd` クエリ中 peak RSS | 18.8 MiB | 20.2 MiB | 参考 |

GUI の数値は Linux/Xvfb（GPU なし、ソフトウェア描画でテクスチャがプロセスメモリに載る）の
もので、Metal の macOS とは計上が異なる。スクリーンショット
（[idle](ISSUE_30/linux/gui-idle.png)、[PDF 表示](ISSUE_30/linux/gui-pdf-open.png)、
[20/20 page](ISSUE_30/linux/gui-pdf-paged.png)）で各段階の画面に到達したことを確認した。
PDF 段階は GUI 自身の「Open paper:// Link from Clipboard」で 1 本目の PDF を開き、Expand、
ホイール 60 回、次 page ボタンで 20 page 目まで送った後の値。

## 索引作成（20,000 page）

| | Linux | macOS VM |
|---|---:|---:|
| 抽出ジョブ期間 | 55.3 s（≈362 page/s） | 40.0 s（≈500 page/s） |
| CPU（rusage） | user 46.9 s / sys 3.0 s | user 29.8 s / sys 5.5 s |
| papersd peak RSS（sampled） | 26.8 MiB | 45.1 MiB |
| 書き込み（`/proc/<pid>/io`） | write_bytes 3.13 GB、wchar 3.71 GB | **未測定** |
| 結果 | 1,000 添付すべて complete、20,000 page に text | 同左 |
| DB | library.sqlite 402.0 MB（WAL 0）、うち検索系 369.3 MB | 同一 |

索引内訳（dbstat）: `fulltext_pages` 82.1 MB、`search_documents` 92.4 MB、
`search_fts_trigram_data` 143.2 MB、`search_fts_words_data` 46.2 MB。抽出テキストは
46.9 MB（UTF-8）。シード 10,000 件の直接 SQL は 12.0 s（Linux）/ 8.9 s（macOS）、
1,000 PDF の添付は 1.5 s / 4.2 s。

## 日本語短語の再現率・対象範囲・p95

正解は生成テキストの literal 部分一致（page 単位）。偽陽性は全語で 0。

| 語（文字数） | 正解 page | 既定 `Bounded` | `AllPages`（完全走査） | 既定 p95（Linux / macOS） |
|---|---:|---|---|---:|
| `電`（1） | 5,765 | page 0 件・`PdfPagesForShortTerms` を明示（部分回答） | 再現率 1.0、15 window・65 call・6.4 s / 5.9 s | 59.2 / 59.6 ms |
| `電池`（2） | 2,858 | 同上 | 再現率 1.0、15 window・35 call・3.5 s / 3.3 s | 57.5 / 62.4 ms |
| `電解質`（3） | 1,730 | 再現率 1.0（trigram） | 同左 | 14.7 / 21.5 ms |
| `固体電解質`（5） | 207 | 再現率 1.0 | 同左 | 9.5 / 6.8 ms |
| `lithium` / `dendrite` / `zirconia` | 4,000 / 400 / 21 | 再現率 1.0 | 同左 | 47.2 / 16.6 / 6.1 ms（Linux） |

3 文字以上は FTS で既定 scope のまま完全。1–2 文字は既定では書誌・注釈のみを走査し
PDF page を「未検索」と返す（0 件表示と区別、#18 の設計どおり）。`AllPages` では
2,000 文書/window の continuation で全 page を走査し再現率 1.0。小規模の常時回帰は既存の
`cm-storage fts::measure`（120 文献）。

## 未達・未測定と調整案

1. **PDF 1本閲覧時の総 RSS（Linux 409.7 MiB > 350 MB、未達）** → #85 で 263.4 MiB に削減（[下記](#85-pdf-閲覧時のメモリ削減)）
   - 根拠: 旧 192 MB キャッシュで 20 page を送ると GUI 573.3 MiB + papersd 89.1 MiB
     = 662.4 MiB（[cache192.json](ISSUE_30/linux-render-cache/cache192.json)）。
     各 bitmap（fit width 218%、約 9.5 MiB）が `RenderCache` の CPU 画像と、描画後の
     GPU atlas テクスチャ（Xvfb ではプロセスメモリ）の二重に保持される。
   - 実施した調整: `CACHE_BUDGET_BYTES` を 64 MB（§16.2 の下限）に変更。同じ library・同じ
     操作で GUI 319.1 MiB（peak 327.2）+ papersd 89.8 = 408.8 MiB
     （[cache64.json](ISSUE_30/linux-render-cache/cache64.json)、20/20 page 表示も確認）。
     調整後コードでの通し測定（linux-final）は GUI 320.6（peak 336.0）+ 89.1 = 409.7 MiB。
     idle からの GUI 単体の増分は +412 MiB（旧、161.1→573.3）→ +159 MiB（新、159.7→319.1）。
     4 Mpx 上限の page でも表示 2 + 前後 prefetch を保持できることを
     `default_render_cache_budget_is_within_the_design_range` で固定。
   - 残差の原因と案: (a) papersd 側 89 MiB は PDFium の文書状態と RGBA を JSON/base64 で
     返す一時領域（glibc が解放後も保持）。描画結果を JSON ではなく binary frame / 共有メモリ
     で渡す、または描画後に allocator を trim する。(b) GUI 側は CPU 画像と GPU テクスチャの
     二重保持。アップロード後に CPU 側を捨てる設計、または予算をテクスチャ込みで数える。
     (c) macOS（Metal、統合メモリ）での計上は未測定のため、16 GB 実機で `footprint`/RSS を測る。
2. **フレーム落ち（未測定）**: GPUI の frame time を取り出す計測点がなく、Xvfb の
   ソフトウェア描画は実機の代替にならない。macOS 実機で Instruments（Core Animation FPS）か
   GPUI の frame callback に計測フックを足す必要がある。
3. **16 GB Apple Silicon 実機（未測定）**: 同じ `benchmark.yml`/`representative.py` を実機で
   `--out` 付きで実行すれば同形式の JSON が得られる。GitHub runner は 7 GB の仮想 M1 で
   代替ではない。
4. **短語（1–2 文字）の collection 内検索**（参考、目標外の製品所見）: collection 0 は
   2,000 文献 + 2,000 page。短語走査の window は文書 id 順で、先に投入された書誌 2,000 件で
   既定 window（2,000）を使い切るため、1 call 目は page を 1 件も見ずに 0 件 +
   `DocumentsAfterWindow` を返す（Linux/macOS 共通、p95 69/89 ms）。完全回答には
   continuation が要る。案: 短語走査を page 文書から先に行う、または narrowing 時は対象
   文書数で window を決める（#18/#21 の範囲、本 PR では変更しない）。
5. **索引作成の書き込み量**: 402 MB の DB に 3.13 GB を書いた（約 7.8 倍、WAL/checkpoint と
   FTS segment merge）。案: 一括抽出時の FTS `automerge`/`crisismerge` と WAL checkpoint 間隔の
   調整、または page batch を大きくする。SSD 寿命より時間への影響を実機で確認する。
6. **メタデータ no-hit**: 10,000 件の literal 全走査で p95 115 ms（Linux）。目標内だが余裕は
   35 ms。件数が増えるなら書誌 FTS（既存 `search_fts_words`）への置換を検討。

## 並列数・キャッシュ上限・fuzz の判断

- **PDF 描画キャッシュ**: 上記のとおり 64 MB / 48 entries（LRU、byte と件数の両方で上限、
  evict 時に GPU テクスチャも解放）。既存 `cache_is_lru_bounded_by_bytes_and_entries` と
  新規テストで固定。
- **抽出**: PDFium は 1 worker thread で直列（`QUEUE_CAPACITY` 8、batch 60 s 予算）、抽出ジョブは
  1 run 500 添付・20 分予算、64 page/commit。20,000 page を 55 s / 40 s で処理、papersd の
  RSS は 27–45 MiB に収まった。並列化は PDFium の thread 安全性と RSS 予算を崩すため不要と判断。
- **ジョブ全体**: `JobEngine` は 1 runner thread で 1 job ずつ。大量抽出中は
  `capture.pdf_fetch` や `attachment.import` も待つ（本測定では最長 1 run ≈ 28 s）。
  実 PDF では run が長くなりうるため、対話的ジョブ用の別 lane、または抽出 run を添付ごとに
  譲る調整を提案（本 PR では変更しない）。
- **取得**: `capture.pdf_fetch` は上記 runner で直列、1 回の取得はサイズ/時間/MIME/リダイレクト
  上限付き（#22）。検索読取は `MAX_SEARCH_READERS` 4、REST は同時 32 要求。
- **fuzz**: coverage-guided fuzzing（cargo-fuzz/libFuzzer）は nightly toolchain と新規
  依存が必要で、[DEPENDENCIES](DEPENDENCIES.md) の固定方針（1.99.1、完全一致 pin）に反するため
  **今回は導入しない**。代わりに依存なしの seed 付き変異試験を通常 CI に入れた:
  BibTeX/RIS/HTML の全 fixture（300 回/件、大きな fixture は 12 回）と IPC frame（240 接続）。
  PDF パーサは PDFium（上流で OSS-Fuzz 済み）を pin しており、自前 PDF parser はない。
  今後 nightly の別 workflow で `bibtex::parse`/`ris::parse`/`html::extract`/IPC frame decode を
  対象に cargo-fuzz を定期実行することを推奨する。

## 回帰テストの追加と既存テストの対応

| 観点 | 新規 | 既存（再利用） |
|---|---|---|
| 強制終了 → DB/job | `sigkill_during_ipc_writes_then_integrity_orphans_backup_and_restore`: 実 papersd へ IPC 書込を流しつつ SIGKILL、再起動後に確認済み書込がすべて連続して残り、未確認は高々 1 件、完了済みジョブは再実行されない | `daemon_lifecycle`（lock/socket/lease 回収、retry_wait 中は idle 終了しない）、`daemon_jobs`、`crash_recovery`（8 checkpoint） |
| 孤立添付 | 同上: 停止中の library に private な content-addressed blob を置くと `check_integrity` が orphan として報告し削除しない | storage `recovery`、#9 inventory |
| バックアップ復元 | 同上 + migration シナリオ: `backup_library`→`validate_bundle`→`restore_library` した新 library を papersd が同じ内容で配信。orphan は可搬 bundle に入らない | storage `recovery`（WAL/manifest/破損 bundle） |
| migration failure | `migration_history_failure_refuses_start_and_backup_restore_recovers`: 適用済み migration の checksum 不一致（1 桁改変）で papersd は exit 1、DB は 1 byte も変わらず backup も増えない。bundle から復元して再開 | storage `refused_migration_snapshot…`、`applied_checksum_future_versions…`（失敗する migration step 自体） |
| 索引破損 | `damaged_search_index_found_over_ipc_is_rebuilt_by_a_daemon_job`: 停止中に両 FTS5 の structure record を破壊、IPC 検索が「damaged; rebuild job scheduled」を返し、`search.rebuild` が 1 回だけ成功、同じ検索結果に戻り元データ不変 | storage `corrupted_fts_index_is_detected_and_rebuilt…`、`busy_writer_is_reported_as_contention_not_damage` |
| 難しい PDF | `layouts_runtime`: 2段組（列ごとに順序保持）、数式（上付き・Symbol→Unicode ギリシャ文字）、罫線表（行優先 9 cell）、縦書き日本語（UniJIS-UCS2-V、各列が連続）、/Rotate 90（横長描画）。全 page を描画も確認 | `extract_runtime`（回転 0/90/180/270、画像のみ page、暗号化、破損）、`fulltext_runtime` |
| 悪意ある HTML/JSON | `mutation_lite`: 変異した HTML から出る PDF 候補は必ず fetch policy の URL 検査を通る、parser は panic しない | `malicious_page_is_inert…`、REST `invalid_bodies…`、MCP `malformed_input…`、native host message 試験 |
| SSRF | （既存で十分） | `fetch_policy` の非公開 IP・全 redirect/DNS 再検査・資格情報なし、capture の公開 PDF のみ |
| 認可迂回 | `hostile`: read-only grant で変異/正規の書込・grant 発行 frame を送っても全て拒否、変異 credential は認証されない、papersd は同一プロセスのまま | `daemon_auth`、`daemon_rest`（全 endpoint の 401/403/失効/Host/Origin）、`daemon_api`、MCP stdio |
| PDFium 以外の注釈表示 | （再実行のみ） | `validate_export.py`（pypdf 構造 + poppler `pdftoppm` 描画、4 回転）を本環境で再実行し `status: passed`（[validate_export.json](ISSUE_30/linux/validate_export.json)、poppler 24.02.0） |
| host/daemon 寿命・lease/retry_wait | （再実行のみ） | `daemon_lifecycle`、`crates/native-host/tests/host_process.rs`（stdin close/idle で host 終了、job は daemon が完了） |

**縦書きの制限（所見）**: PDFium 8125 は非埋め込みフォントの Identity-V（ToUnicode 付き）
テキストを抽出しない（文字が 0 件）。fixture は非埋め込み日本語の定番である
UniJIS-UCS2-V を使った。埋め込みフォントの Identity-V は fixture 化にフォント配布が要るため
**未検証**。また縦書き 2 列は x 昇順（左の列から）に出て、右→左の読み順とは逆になる。
検索は列内の連続性に依存するため問題ないが、抽出テキストの読み順は保証しない。

**システム CJK フォント依存（CI で確認）**: 非埋め込みの HeiseiMin（UniJIS-UCS2-V）縦書きは、
PDFium が代替のシステム CJK フォントを見つけられないと抽出結果が空になる。素の ubuntu runner では
`layouts_runtime` が失敗し、`fonts-wqy-zenhei` を入れると成功した（PR #77 の CI）。macOS は標準で
日本語フォントを持つ。Linux で縦書き PDF を検索対象にするには CJK フォントの導入が必要（INSTALL 系文書で案内する）。

## 実行したコマンドと結果

Linux（本 worktree）で最終ソースに対して実行した exit code。新規の `daemon_recovery` と
`daemon_hostile` は追加で 3 回連続実行し全て passed（不安定さなし）。

| コマンド | exit |
|---|---|
| `cargo fmt --all -- --check` | 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 |
| `cargo clippy -p cm-desktop --all-targets --features native-gpui --locked -- -D warnings` | 0 |
| `cargo clippy -p cm-daemon --all-targets --features desktop-bootstrap --locked -- -D warnings` | 0 |
| `cargo test --workspace --locked --no-fail-fast` | 0（580 passed、0 failed、0 ignored） |
| `cargo test -p cm-daemon -p cm-desktop --features cm-daemon/desktop-bootstrap --locked` | 0（229 passed） |
| `python3 scripts/check_architecture.py` | 0 |
| `python3 scripts/check_migrations.py --base origin/main` | 0 |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | 0（44 tests） |
| `CM_PDFIUM_LIBRARY=… cargo test -p cm-pdf --features runtime --locked --test metadata_runtime --test runtime --test extract_runtime --test export_runtime --test layouts_runtime` | 0（5 passed） |
| `CM_PDFIUM_LIBRARY=… cargo test -p cm-daemon --features pdfium-runtime-tests --locked --test fulltext_runtime --test pdf_export_runtime` | 0（2 passed） |
| `scripts/pdfium/validate_export.py target/pdf-export-test-*`（pinned requirements の venv、poppler 24.02.0） | 0 |
| `python3 scripts/benchmarks/representative.py --gui --out linux-final --pdfium <pinned libpdfium.so>` | 0 |
| `benchmark.yml`（macos-15、workflow_dispatch） | success |

migration は追加・変更していない。新規依存なし（`Cargo.lock` 不変）。重い代表負荷は
`cargo test` にも PR CI にも入れていない（examples の小さな単体テストのみ通常 CI で実行）。
CI の `linux-native` job は新しい `layouts_runtime` も pin 済み PDFium で実行する。

## #85 PDF 閲覧時のメモリ削減

2026-11-25、implementing agent が branch `claude/issue-85-pdf-memory`（base `2cc9074` = origin/main）で実施。
生データは [ISSUE_30/linux-m62/](ISSUE_30/linux-m62/)（ローカル絶対パスは `<scratch>`/`<worktree>`/`<tmp>` に置換）。

### 結果（Linux、#30 と同じ手順）

`representative.py --gui`（1万文献/1000 PDF/2万 page、同じ GUI 操作: paper:// link で 1 本目の PDF、
Expand、ホイール 60 回、次 page ×24）。同じマシンで before（origin/main）と after を測った。

| 段階 | before GUI + papersd (MiB) | after GUI + papersd (MiB) |
|---|---:|---:|
| idle（PDF なし） | 161.0 + 13.1 | 160.0 + 13.3 |
| PDF を開いた直後 | 175.6 + 39.3 | 167.0 + 24.9 |
| 60 回スクロール後 | 304.1 + 91.3 | 210.8 + 25.2 |
| 20/20 page まで送った後 | 351.8 + 91.3 | 238.2 + 25.2 |
| **PDF 1 本閲覧時の総 RSS（各段階の最大）** | **443.2** | **263.4（< 350 MB、達成）** |

- before の総 RSS は #30 記録（409.7 MiB）より大きいが、同じ origin/main コードで本マシンで再測定した値を
  比較の基準にした（GUI 側の差。共有ホストで他 agent の負荷は制御外）。
- after は [after-summary.json](ISSUE_30/linux-m62/after-summary.json)、[after-gui.json](ISSUE_30/linux-m62/after-gui.json)。
  スクリーンショット（[PDF 表示](ISSUE_30/linux-m62/after-gui-pdf-open.png)、
  [スクロール後](ISSUE_30/linux-m62/after-gui-pdf-scrolled.png)、[20/20 page](ISSUE_30/linux-m62/after-gui-pdf-paged.png)）で
  各段階に到達し、ページが描画されていることを確認した。before の同段階
  （[before-gui-pdf-scrolled.png](ISSUE_30/linux-m62/before-gui-pdf-scrolled.png)）と画素単位で同一だった。
- headless の新しい段階（`representative_ipc renders`: 1 本目の PDF 20 page を zoom 2.2、3 接続で IPC 描画）:
  papersd 11.9 → 25.3 MiB（2 秒後、sampled peak 34.5 MiB）。macOS runner でも同じ段階を測った（下記）。
- その他の指標は after でも目標内（warm 起動 p50 35.5 / p95 40.3 ms、collection 切替 p95 5.9 ms、
  メタデータ p95 102.2 ms、全文 p95 34.3 ms、GUI idle 160.0 MiB）。re-exec（下記）による
  warm 起動の悪化は見られない。before 実行の遅延系の値は、測定中に本 worktree の build を並行させた
  ため比較に使わない（RSS の GUI 段階は build なしで別途実行）。

### 原因の特定（測定）

測定の手順は 2 つ。(1) GUI 段階の最後に `/proc/<pid>/smaps`（mapping 種別ごとの Rss）を取り、
gdb で `malloc_trim(0)` を呼んで前後の RSS を比べる（glibc が free 済みなのに保持している量）。
(2) papersd を単独で起動し、IPC client で 1 本目の PDF の 20 page を描画して（zoom 1.7、3 件同時）、
各応答後の RSS を見る。allocator 設定を変えた実行で比べる。

papersd（単独描画、開始時 11.7 MiB、20 page 後）:

| 条件 | 20 page 後 | malloc_trim 後 |
|---|---:|---:|
| before | 74.6 MiB（9 page 目以降は横ばい） | 64.5 MiB |
| before、`GLIBC_TUNABLES=glibc.malloc.mmap_threshold=262144` | 24.6 | — |
| before、`glibc.malloc.arena_max=1` | 59.1 | — |
| before、同時 1 件 | 40.9 | — |
| binary frame 化のみ（閾値は既定） | 40.0 | 29.1 |
| after（binary frame + 固定閾値） | 24.6 | 19.1 |

- 応答 frame は before が 7.9 MB の JSON（base64）、after が 6.0 MB（生 RGBA + 小さな JSON）。
  before は 1 page ごとに PDFium の bitmap、BGRA→RGBA の Vec、`Arc<[u8]>` へのコピー、base64 文字列、
  JSON 本体、frame バッファと、page の数倍の一時領域を確保していた。
- **保持の主因は glibc の動的 mmap 閾値**: 大きな mmap 領域が free されると閾値がそのサイズ
  （最大 32 MiB）まで上がり、以後の bitmap 大の確保は heap（thread arena）から取られて free 後も
  OS に返らない。閾値を固定すると 74.6 → 24.6 MiB。`malloc_trim` では 10 MiB しか戻らず、
  arena 数を 1 にしても 59.1 MiB 残る（断片化した heap に残るため）。
- 残る約 13 MiB（11.7 → 24.6）は PDFium 側（文書を閉じた後のフォント等のキャッシュ）で、
  描画回数に比例しない。

GUI（20/20 page 段階の smaps、KiB）:

| mapping | before | after |
|---|---:|---:|
| memfd（Mesa lavapipe の device memory = atlas テクスチャ、swapchain） | 99,396 | 99,424 |
| heap（brk） | 93,584 | 23,764 |
| 匿名 mmap | 56,024 | 9,908 |
| libLLVM（lavapipe の JIT、file-backed） | 55,352 | 55,388 |
| `malloc_trim` で戻った量 | 41.4 MiB | 1.8 MiB |

- before は描画キャッシュの CPU 側画像（最大 64 MB）が GPU テクスチャと二重に残り、加えて
  一時領域（受信 frame、base64 文字列、デコード後 Vec、Arc コピー、回転用コピー）が heap に残った。

寄与の切り分け（同じ library・同じ GUI 操作、総 RSS = GUI + papersd の最大）:

| 構成 | 総 RSS (MiB) | 証跡 |
|---|---:|---|
| before | 443.2 | before-gui.json |
| before + 固定閾値（環境変数のみ） | 318.9（293.9 + 25.0） | variant-tunable.json |
| after のコードから re-exec だけ外したもの | 292.5（267.4 + 25.1） | variant-notune.json |
| after（同じ library での GUI 段階） | 262.3（237.2 + 25.1） | variant-v1.json |
| after（通しの再測定） | 263.4（238.2 + 25.2） | after-summary.json |

### 変更内容

1. **IPC の binary frame（protocol version 2）**: 描画済み page の RGBA を JSON/base64 ではなく frame の
   binary 部で送る。frame は従来どおり 4 byte 長 + 本体で、上限（応答 64 MiB、要求 8 MiB、
   handshake 64 KiB）は本体全体に掛かる。本体が `0x00` で始まる場合は
   `json_len | count | 各 blob 長 | JSON | blob…`。JSON 側は `{"blob": i}` で参照する。部分の合計と
   frame 長の一致（payload を確保する前に検査）、count 1..=8、全 blob をちょうど 1 回参照、を満たさない
   frame は malformed として接続を閉じる。受信側は各 blob を最終的に保持する buffer に直接読み、
   送信側は元の buffer から書く。handshake は平文 JSON のままなので、version 1 の client は
   `VersionMismatch{supported: 2}` を受け取れる。認可は従来どおり request ごとで、binary 形式でも
   認証前の request は実行されない（`daemon_ipc` で確認）。REST（`GET /attachments/{id}/pages/{page}`
   の base64 JSON）と MCP は IPC frame を経由しないため契約は不変（OpenAPI 契約 test を含む既存 test が通る）。
   `RenderedPage.rgba` は frame 外の JSON では従来どおり base64 文字列になる。
2. **PDFium が RGBA を直接書く**（`FPDF_REVERSE_BYTE_ORDER`）: BGRA→RGBA の変換コピーをなくした。
   daemon 側の `Arc` への再コピーもなくした（`rgba` は `Arc<Vec<u8>>`）。
3. **GUI: GPU 転送後に CPU 側の画素を解放**: page は canvas で `paint_image` し、成功した page に
   upload 済みの印を付ける。16 ms ごとの message pump で、その page の `RenderImage` を同じ image id を持つ
   1x1 の代替に置き換える。CPU 側の画素は解放され、atlas のテクスチャがそのまま表示を続ける。テクスチャは
   cache entry と一緒にしか解放しないため、スクロールで戻る・再描画するときはテクスチャから描かれる。
   evict・回転・文書切替で entry が消えた page は再描画を要求し、代替画像から描くことはない。
   回帰テスト `drawn_pages_release_their_cpu_pixels_and_redisplay_from_the_texture` でこれを確認している。
   受信 buffer は GPUI 画像にそのまま変換し、回転なしではコピーしない。
   付随して、render 中・クリック処理中に `drop_image(_, None)` を呼ぶと、更新中の window の atlas に
   届かずテクスチャが残る問題があった（回転・文書切替でリーク）。テクスチャの解放も message pump から行うよう直した。
4. **glibc の mmap 閾値を固定**（`cm_daemon::malloc_tuning`）: papersd と GUI は、glibc の Linux で
   起動直後に一度だけ `GLIBC_TUNABLES=glibc.malloc.mmap_threshold=262144` を付けて自分を re-exec する
   （`/proc/self/exe`、PID・引数・fd は不変）。`mallopt` は FFI（unsafe）で workspace の方針に反するため、
   この方法にした。利用者が閾値を設定済みなら尊重し、exec に失敗しても既定のまま動く。子プロセスは
   この変数を継承するが、変わるのは allocator の閾値だけ。macOS・他の libc では何もしない。

### Xvfb での手動確認（after のコード）

`citation-manager` を Xvfb（1400x900x24）で起動し、xdotool で操作した。使った build は製品コードが
after と同一のもの（test・文書・harness の変更前）。
- PDF 表示、スクロール、次 page ×12、上方向へ 200 回スクロール（解放済み page の再表示を含む）。
- zoom +2 段（400%）、−4 段（125%）、Width。
- 回転 1 回、その後 3 回で元に戻す。
- page 2 へジャンプし、1 行目をドラッグ選択して Highlight。次 page/前 page ×4 の後もハイライトが残る。
- 1 ページ目を表示したまま 6 回下へスクロールし、3 秒待ってから 6 回戻した。この画面は、最初の画面と
  page 領域が画素単位で同一だった（差分は「Exit reading」ボタンの hover 表示のみ）。

いずれもページが空白にならず正しく描画された。スクリーンショット:
[解放後の再表示](ISSUE_30/linux-m62/manual-redisplay-released-page.png)、
[上方向スクロール](ISSUE_30/linux-m62/manual-scrolled-back.png)、
[zoom in](ISSUE_30/linux-m62/manual-zoom-in.png)、[zoom out](ISSUE_30/linux-m62/manual-zoom-out.png)、
[回転](ISSUE_30/linux-m62/manual-rotated.png)、[ハイライト](ISSUE_30/linux-m62/manual-highlight.png)。

### macOS runner（headless）

`benchmark.yml`（macos-15、`VirtualMac2,1` Apple M1 Virtual、3 vCPU、7 GiB）を本 branch で
workflow_dispatch した。run [45636732626](https://github.com/ks250206/citation-manager/actions/runs/45636732626)
（head `6b9d5f5`、[macos-m62/](ISSUE_30/macos-m62/)）と、先行 run
[45633567308](https://github.com/ks250206/citation-manager/actions/runs/45633567308)
（head `69a2d1d`、同じ製品コード、`run1-*.json`）。どちらも成功。

| 指標 | run 45636732626 | run 45633567308 | #30（`ee2a33b`） |
|---|---:|---:|---:|
| papersd: 20 page 描画後の `ps` RSS | 71.0 MiB（開始時 8.7） | 71.8 MiB（開始時 8.9） | — |
| 同、phys_footprint（`footprint`/`vmmap`） | **7.1 MB**（peak 29 MB） | 未取得 | — |
| warm 起動 p50 / p95 | 122.6 / 241.8 ms | 176.7 / 289.5 ms | 121.0 / 209.5 ms |
| collection 切替 p95 | 9.8 ms | 7.6 ms | 3.2 ms |
| メタデータ検索 p95 | 137.8 ms | 129.6 ms | 77.2 ms |
| 全文検索 p95 | 62.7 ms | 43.3 ms | 28.8 ms |
| 索引中 peak / クエリ中 peak RSS | 46.5 / 19.9 MiB | 47.0 / 20.0 MiB | 45.1 / 20.2 MiB |

- **macOS の `ps` RSS 71 MiB は、解放済みの大きな割り当てを libmalloc がキャッシュしている分**:
  `vmmap` では `MALLOC_LARGE_REUSABLE` が 42.3 MB（resident だが reusable、footprint には数えない）で、
  phys_footprint は 7.1 MB だった。OS が必要に応じて回収するので、Activity Monitor の「メモリ」
  （footprint）では保持にならない。glibc の固定閾値に相当する対処は macOS では不要と判断した。
  binary frame 化前の macOS 値は、この段階が protocol 2 専用のため**未測定**。
- 遅延系の値は、#30 時点と 2 本の run の間で一様にばらついた（本変更が通らない経路も含めて 30–100%）。
  共有 VM runner の差と判断し、目標（§16.2）はすべて満たしている。
- **GUI の RSS は macOS では未測定**（`benchmark.yml` は headless のみで、GUI を操作する手段がない）。
  re-exec は glibc 専用で macOS では何もしない。

### 実行したコマンドと結果（#85）

Linux（本 worktree、最終 head のソース）での exit code。

| コマンド | exit |
|---|---|
| `cargo fmt --all -- --check` | 0 |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | 0 |
| `cargo clippy -p cm-desktop --all-targets --features native-gpui --locked -- -D warnings` | 0 |
| `cargo clippy -p cm-daemon --all-targets --features desktop-bootstrap --locked -- -D warnings` | 0 |
| `cargo test --workspace --locked --no-fail-fast` | 0（606 passed、0 failed、0 ignored） |
| `cargo test -p cm-daemon -p cm-desktop --features cm-daemon/desktop-bootstrap --locked` | 0（247 passed） |
| `cargo build -p cm-daemon --bin citation-manager --features desktop-bootstrap --locked` | 0 |
| `python3 scripts/check_architecture.py` | 0 |
| `python3 scripts/check_migrations.py --base origin/main` | 0 |
| `python3 -m unittest discover -s scripts -p 'test_*.py'` | 0（54 tests） |
| `CM_PDFIUM_LIBRARY=… cargo test -p cm-pdf --features runtime --locked --test metadata_runtime --test runtime --test extract_runtime --test export_runtime --test layouts_runtime` | 0（5 passed） |
| `CM_PDFIUM_LIBRARY=… cargo test -p cm-daemon --features pdfium-runtime-tests --locked --test fulltext_runtime --test pdf_export_runtime` | 0（2 passed） |
| `python3 scripts/benchmarks/representative.py --gui --out m62-final --pdfium <pinned libpdfium.so>` | 0 |
| `benchmark.yml`（macos-15、workflow_dispatch、2 回） | success |

migration の追加・変更なし、新規依存なし（`Cargo.lock` 不変）。

### 未達・未測定・制限

- Linux の GUI 値は Xvfb のソフトウェア描画（Mesa lavapipe）によるもの。テクスチャ（memfd 約 97 MiB）と
  LLVM（約 54 MiB、file-backed）がプロセス RSS に計上される。GPU のある実機では、テクスチャは VRAM または
  統合メモリに置かれ、計上のされ方が異なる。
- GPU renderer が atlas を消去した場合（device loss、GPU エラーの連続）、解放済みの page は、cache から
  外れるまで 1x1 の代替で描かれる（zoom や回転を変えれば再描画される）。この経路は Xvfb では再現できず、
  **未検証**。
- さらに減らす案: 描画キャッシュ予算を 64 → 32 MB にする（テクスチャを直接減らす）、画面外のテクスチャを
  早めに解放する、lavapipe 環境でだけ swapchain の枚数を減らす。いずれも今回の目標には不要なので実施していない。
