# #86 M6-3 体感性能の計測（フレーム時間・PDF 初回表示・ページ送り）

2026-11-25、implementing agent が worktree `claude/issue-86-frame-timing` で実施。
#30 で「未測定（フレーム時間を取る手段がない）」とした項目を測るための仕組みを入れ、
現状値を 60 Hz（16.7 ms）/ 120 Hz（8.3 ms）の予算と比べて記録する。生データは
[ISSUE_86/](ISSUE_86/)、手順は [scripts/benchmarks/README.md](../../scripts/benchmarks/README.md#86-gui-frame-timing-perceived-performance)。

**最重要の制約:** 以下はすべて仮想マシン上の値で、実機の体感ではない。
Linux は Xvfb + lavapipe（Mesa の CPU 実装 Vulkan、GPU なし）のソフトウェア描画、macOS は
GitHub Actions の Apple M1 **仮想** runner（paravirtualized GPU、1024x768 の仮想ディスプレイ）。
present 側の値はとくに実 GPU・実ディスプレイと大きく異なりうる。DESIGN §16.1 の測定機
（Apple Silicon、16 GB RAM）では未測定。

## 受け入れ条件と証跡

| 受け入れ条件 | 状態 | 証跡 |
|---|---|---|
| GUI にフレーム時間を記録するフック。既定で無効、環境変数で有効 | 達成 | `crates/desktop/src/frame_timing.rs`（記録器・単体テスト 5 件）、`crates/desktop/src/native/frame_probe.rs`、`native/mod.rs`・`native/pdf_viewer_ui.rs` のフック呼び出し |
| 1 万件一覧のスクロールを p50/p95/最大で出力 | 達成（Linux/Xvfb、macOS VM）。現行 UI は 50 件ずつの paging のため、ページ内スクロールとページ切替を測定（制限 4） | 下表 `list_wheel`/`list_keys`/`list_pages` |
| PDF の初回表示までの時間 | 達成（Linux/Xvfb）。macOS は下記参照 | `pdf_first_page` span と入力→初回表示 |
| ページ送り | 達成（Linux/Xvfb）。macOS は下記参照 | `page_turn` span（隣接ページ J/K、離れたページへのジャンプ） |
| タブ切り替え（M6-11 の後） | **未実施**（M6-11 未着手。タブが存在しない） | — |
| `workflow_dispatch` のワークフローと手順書で実行、PR ごとの CI には含めない | 達成 | `.github/workflows/benchmark.yml`（`suite` 入力、`frame-timing-linux`/`frame-timing-macos` ジョブ。トリガーは `workflow_dispatch` のみ）、`scripts/benchmarks/frame_timing.py`、README |
| 16.7 ms / 8.3 ms を基準に現状値を `docs/validation/` に記録 | 達成（本書） | 本書の表、[ISSUE_86/](ISSUE_86/) |

## 記録フック（GUI 側）

- **有効化**: GUI 起動時に `CM_FRAME_TIMING=<絶対パス>` があるときだけ記録する。未設定・空なら
  無効、相対パスは警告を出して無効。無効時は各フックが `OnceLock` の読み出し 1 回と分岐だけで、
  確保・計時・ファイル書き込みは一切しない（`disabled_unless_an_absolute_path_is_given`）。
  リリースビルドでも同じ（ビルド時の feature では切り替えない）。
- **出力**: 指定ファイルだけ。別スレッドが約 1 秒ごとに新しいエントリを取り出し、
  `<path>.tmp` に書いてから rename する（UI スレッドはメモリ上のバッファに追記するだけ）。
  終了操作（Quit）時にも 1 回書く。上限はフレーム 500,000・span 100,000（超過分は件数だけ数える）。
- **フレーム**: ルートビュー `render` の開始から、ツリー末尾に置いたマーカー要素の paint までの
  アプリ側の仕事（render + layout + prepaint + paint）。GPU/コンポジタへの present は**含まない**。
- **span**:
  - `list_scroll` / `pdf_scroll`: 一覧（`uniform_list`）または PDF ページ列のスクロール位置が
    前フレームから動いたフレームで開始、最後に動いたフレームから 300 ms 動かなければ終了。
  - `pdf_first_page`: PDF ビューアが作られた時点 → 表示範囲内のページの画像を初めて paint した
    フレームの終わり。
  - `page_turn`: ページジャンプ（ツールバー ‹/›、ページ欄、vim `J`/`K`、パレットの先頭/末尾）
    → 目的ページが表示範囲内で画像付きで paint されたフレームの終わり。完了前に次のジャンプが
    来たものは `completed: false`（集計から除外し件数だけ出す）。
- 時刻はすべて単調時計の原点からの µs と、その原点の Unix 時刻。ハーネスが自分の入力時刻と
  突き合わせられる。
- 変更範囲は他 issue（#84 一覧、#85 PDF 転送）との衝突を避けて最小にした: 新規 2 ファイル、
  `native/mod.rs` に 10 行（render 先頭の計時・一覧オフセット観測、終端マーカー、初期化）、
  `native/pdf_viewer_ui.rs` に 17 行（1 行置換）（ビューア生成・ジャンプ・描画済みページの通知）。
  `viewer_jump` は `scroll_to_page` 直呼びから `Viewer::jump` 経由に変えた（挙動は同じ）。

## 計測ハーネス

`scripts/benchmarks/frame_timing.py`（#30 の `representative.py` の build/stage/seed/索引を再利用）:

1. release の `citation-manager`（`--features desktop-bootstrap`）・`papersd`
   （`--features pdf-metadata`）をビルドし、pin 済み PDFium を製品と同じ配置に置く。
2. #30 と同じ生成器で 10,000 文献 / 1,000 PDF / 20,000 page（seed 30）の library を作り、
   `papersd` に全文索引まで終わらせる（GUI 計測中に抽出ジョブが CPU を奪わないように）。
3. GUI を `CM_FRAME_TIMING=<run>/frames.json`・`ZED_MEASUREMENTS=1` で起動する。後者は GPUI 自身が
   frame callback 内の各描画について `Window::draw` + `present` の所要時間を stderr に出すもので、
   ハーネスは各行の**到着時刻**を記録する（`gpui-frames.json`）。
4. 入力を送り、各フェーズの開始・終了 Unix 時刻を記録する（`phases.json`）。フレームはフェーズの
   時間窓で、span は開始時刻で振り分ける（`summary.json`、分位点は nearest rank）。

| フェーズ | 操作 |
|---|---|
| `startup` | 起動から 15 s 待機（一覧の先頭ページ読み込み） |
| `list_wheel` | 一覧上でホイール下 40 / 上 40 を 3 回（40 ms 間隔） |
| `list_keys` | 一覧をクリックして vim `j`×80 → `k`×80（60 ms 間隔、選択に詳細ペインが追従） |
| `list_pages` | `Next ›` を 5 回（1.5 s 間隔、1 万件を 50 件ずつ keyset paging） |
| `pdf_open` | 1 本目の PDF の `paper://` リンクをクリップボードに置き、⌘/Ctrl+K →「Open paper:// Link」→ Return（15 s 以内に開かなければパレット 1 行目をクリック。macOS はこちら） |
| `page_turn_forward` / `_back` | Expand（読書レイアウト、fit width: Linux 218%、macOS 168%）後、ページ列をクリックして `J`×19 / `K`×19（700 ms 間隔） |
| `page_jump` | ページ欄に 10, 20, 5, 15, 1, 12, 18, 3 を入力して Return（1.5 s 間隔、キャッシュ外のページが中心） |
| `pdf_wheel` | ページ列上でホイール下 60 / 上 60 を 2 回 |
| `pdf_keys` | vim `j`×60 → `k`×60（50 ms 間隔） |
| `idle` | 5 s 待機後、⌘/Ctrl+Q で終了 |

入力手段: Linux は専用の Xvfb（1400x900x24）上で xdotool/xclip。macOS は ctypes から Quartz
イベント（`CGEventPost`）を送る（runner では `AXIsProcessTrusted=True`）。座標はウィンドウ内容
領域からの相対値で、右端・下端基準も使う（macOS runner ではディスプレイが 1024x768 のため
ウィンドウが 1024x680 に縮む）。期待した span が 1 件も完了しなかったフェーズは
`observed: false` とし、測定済み扱いにしない。

## 環境

| | Linux（本 worktree） | Linux（GitHub runner） | macOS（GitHub runner） |
|---|---|---|---|
| ハードウェア | Intel Xeon @ 2.30GHz、4 vCPU、15.7 GiB、`/dev/dri` なし | AMD EPYC 9V45、4 vCPU（Hyper-V VM） | `VirtualMac2,1`、Apple M1 (Virtual)、7 GiB、`kern.hv_vmm_present=1` |
| OS / 描画 | Ubuntu 24.04.5（Firecracker VM 上のコンテナ）、Xvfb 1400x900x24、Mesa lavapipe | ubuntu-24.04 image、Xvfb、Mesa 25.2.8 lavapipe | macOS 15.8.2、Metal（paravirtualized）、仮想ディスプレイ 1024x768 |
| 隔離 | 他の agent と共有ホスト（同時に別 agent の build が走りうる、制御外） | 共有 runner | 共有 runner |
| 実行 | `linux-local-1`（head `93cc3d2`＋ページジャンプ追加の未コミット差分＝`911f0a3` と同内容）、`linux-local-2`（`911f0a3`＋座標の端基準化＝`6531587` と同内容）。library は同一（10k/1k/20k） | workflow run [45643384427](https://github.com/ks250206/citation-manager/actions/runs/45643384427)（主）と [45639655762](https://github.com/ks250206/citation-manager/actions/runs/45639655762)（繰り返し）、どちらも head `28a3137`（clean）、`suite: frame-timing` | run [45643384427](https://github.com/ks250206/citation-manager/actions/runs/45643384427)（head `28a3137`）。45639655762 の macOS ジョブは runner 不足で開始されず cancelled |

共通: Rust 1.99.1、`--release --locked`、opt-level 3、debug assertions なし。すべての実行で
Rust のソースは同一（`93cc3d2` 以降の変更はハーネスと文書だけ）。ローカルの 2 回は、最初の
ローカル実行（ページジャンプ追加前のハーネス、結果は不採用）が作った library を `--reuse` で
使い回した。

## 結果（10,000 文献 / 1,000 PDF / 20,000 page）

「app」は記録フックのフレーム（render→paint、present なし）、「GPUI」は GPUI 自身の draw+present
（フェーズ内に到着した行）。「>16.7 / >8.3」はその予算を超えたフレーム数。分位点は nearest rank。

### Linux GitHub runner（run 45643384427、lavapipe ソフトウェア描画）

| フェーズ | app n | app p50 / p95 / max (ms) | >16.7 / >8.3 | GPUI n | draw+present p50 / p95 / max (ms) | >16.7 / >8.3 |
|---|---:|---:|---:|---:|---:|---:|
| startup | 4 | 2.56 / 34.9 / 34.9 | 1 / 1 | 3 | 146.1 / 229.9 / 229.9 | 3 / 3 |
| list_wheel | 203 | 2.13 / 2.94 / 3.85 | 0 / 0 | 203 | 20.6 / 21.6 / 22.6 | 203 / 203 |
| list_keys | 186 | 2.88 / 3.38 / 4.23 | 0 / 0 | 149 | 21.8 / 26.1 / 28.2 | 149 / 149 |
| list_pages | 9 | 6.38 / 6.65 / 6.65 | 0 / 0 | 9 | 25.5 / 56.5 / 56.5 | 9 / 9 |
| pdf_open | 28 | 2.39 / 4.37 / 5.72 | 0 / 0 | 10 | 26.2 / 77.2 / 77.2 | 10 / 10 |
| page_turn_forward | 38 | 1.47 / 4.65 / 4.84 | 0 / 0 | 37 | 41.1 / 45.1 / 45.4 | 37 / 37 |
| page_turn_back | 33 | 1.41 / 2.18 / 2.22 | 0 / 0 | 33 | 39.9 / 44.4 / 45.6 | 33 / 33 |
| page_jump | 65 | 1.26 / 2.60 / 4.46 | 0 / 0 | 48 | 40.4 / 53.6 / 54.4 | 48 / 48 |
| pdf_wheel | 203 | 0.83 / 0.96 / 1.97 | 0 / 0 | 203 | 35.9 / 38.6 / 48.9 | 203 / 203 |
| pdf_keys | 121 | 0.82 / 0.95 / 1.10 | 0 / 0 | 119 | 35.7 / 37.8 / 40.3 | 119 / 119 |
| **全体** | 897 | 2.01 / 3.26 / 34.9 | 1 / 1 | 821 | 33.8 / 43.1 / 229.9 | 821 / 821 |

### Linux 本 worktree（`linux-local-2`、lavapipe、共有ホスト）

| フェーズ | app n | app p50 / p95 / max (ms) | >16.7 / >8.3 | GPUI n | draw+present p50 / p95 / max (ms) | >16.7 / >8.3 |
|---|---:|---:|---:|---:|---:|---:|
| startup | 4 | 5.60 / 15.4 / 15.4 | 0 / 2 | 3 | 56.6 / 58.0 / 58.0 | 3 / 3 |
| list_wheel | 201 | 3.74 / 6.30 / 11.0 | 0 / 1 | 201 | 36.5 / 52.0 / 79.8 | 201 / 201 |
| list_keys | 259 | 4.11 / 6.29 / 8.25 | 0 / 0 | 184 | 36.0 / 43.0 / 53.1 | 184 / 184 |
| list_pages | 8 | 10.7 / 18.7 / 18.7 | 1 / 5 | 8 | 42.0 / 106.3 / 106.3 | 8 / 8 |
| pdf_open | 27 | 3.85 / 8.62 / 31.5 | 1 / 2 | 11 | 58.0 / 116.8 / 116.8 | 11 / 11 |
| page_turn_forward | 38 | 2.56 / 7.29 / 7.84 | 0 / 0 | 37 | 72.9 / 96.9 / 125.7 | 37 / 37 |
| page_turn_back | 33 | 2.46 / 5.76 / 5.99 | 0 / 0 | 33 | 70.2 / 131.3 / 146.8 | 33 / 33 |
| page_jump | 63 | 2.49 / 6.83 / 9.95 | 0 / 1 | 40 | 74.2 / 107.8 / 121.7 | 40 / 40 |
| pdf_wheel | 140 | 1.51 / 2.47 / 6.73 | 0 / 0 | 140 | 61.3 / 86.7 / 108.7 | 140 / 140 |
| pdf_keys | 121 | 1.54 / 2.29 / 10.8 | 0 / 1 | 45 | 61.1 / 74.3 / 90.6 | 45 / 45 |
| **全体** | 901 | 3.40 / 6.35 / 31.5 | 2 / 12 | 709 | 43.2 / 86.1 / 146.8 | 709 / 709 |

`linux-local-1`（同じ library・同じ GUI バイナリ）は全体 app p50/p95/max 3.33 / 7.04 / 40.3 ms
（>16.7: 2、>8.3: 17）、GPUI 29.6 / 90.9 / 155.7 ms。

### macOS GitHub runner（run 45643384427、Apple M1 Virtual、Metal、ウィンドウ 1024x680）

| フェーズ | app n | app p50 / p95 / max (ms) | >16.7 / >8.3 | GPUI n | draw+present p50 / p95 / max (ms) | >16.7 / >8.3 |
|---|---:|---:|---:|---:|---:|---:|
| startup | 5 | 18.4 / 169.2 / 169.2 | 3 / 3 | 4 | 18.9 / 49.5 / 49.5 | 3 / 4 |
| list_wheel | 48 | 5.51 / 10.2 / 13.8 | 0 / 4 | 48 | 5.87 / 11.2 / 14.4 | 0 / 7 |
| list_keys | 316 | 3.90 / 7.44 / 36.6 | 2 / 7 | 4 | 5.44 / 5.54 / 5.54 | 0 / 0 |
| list_pages | 11 | 5.16 / 14.2 / 14.2 | 0 / 2 | 11 | 7.41 / 15.3 / 15.3 | 0 / 3 |
| pdf_open（下記の異常あり） | 30 | 5.22 / 21.1 / 90.7 | 3 / 7 | 12 | 8.54 / 91.4 / 91.4 | 3 / 8 |
| page_turn_forward | 38 | 1.93 / 9.83 / 10.2 | 0 / 4 | 5 | 4.72 / 11.0 / 11.0 | 0 / 2 |
| page_turn_back | 28 | 2.03 / 5.07 / 12.2 | 0 / 1 | 3 | 3.95 / 13.4 / 13.4 | 0 / 1 |
| page_jump | **未観測**（Return が届かず送信されない） | | | | | |
| pdf_wheel | **未観測**（スクロールが発生せず 5 フレーム） | | | | | |
| pdf_keys | **無効**（`j`/`k` がページ送りとして処理された。下記） | | | | | |

macOS の list_keys / page_turn の GPUI 行数が少ないのは、キー入力時の描画が frame callback の外
（`dispatch_key_event` 内の同期 draw）で行われるため（後述）。

### span（入力→表示）

| span | Linux CI 45643384427 | Linux CI 45639655762 | linux-local-1 | linux-local-2 | macOS CI |
|---|---:|---:|---:|---:|---:|
| PDF 初回表示: ビューア生成→初回 paint | 146.4 ms | 147.1 ms | 214.6 ms | 196.9 ms | 49,903 ms（異常、下記） |
| PDF 初回表示: 入力（Return/クリック）→初回 paint | 200.7 ms | 210.4 ms | 324.1 ms | 344.0 ms | 50,194 ms（同上） |
| 隣接ページ送り `J` ×19: p50 / p95 / max | 9.56 / 17.3 / 17.3 | 9.36 / 16.8 / 16.8 | 8.38 / 17.3 / 17.3 | 9.47 / 17.9 / 17.9 | 1.88 / 10.5 / 10.5 |
| 隣接ページ戻し `K` ×19 | 10.2 / 16.4 / 16.4 | 8.95 / 15.8 / 15.8 | 9.28 / 18.0 / 18.0 | 9.51 / 16.2 / 16.2 | 2.10 / 3.64 / 3.64 |
| 離れたページへのジャンプ ×8 | 186.7 / 228.0 / 228.0 | 191.2 / 205.2 / 205.2 | 304.0 / 427.5 / 427.5 | 291.6 / 339.3 / 339.3 | 未観測 |

いずれも完了しなかった span（`completed: false`）は 0 件（macOS の無効な pdf_keys を除く）。
ページ送りの span は「入力イベントを処理した時刻 → 目的ページを paint したフレームの終わり」で、
present と表示までの遅延は含まない。

## 解釈

- **アプリ側の仕事はおおむね予算内。** render→paint の p95 は、Linux で一覧スクロール
  2.9–6.9 ms、PDF のスクロール・ページ送り 1.0–8.3 ms。60 Hz 予算（16.7 ms）超えは全フレームの
  0.1–0.3%（Linux）/ 1.2%（macOS VM）、120 Hz 予算（8.3 ms）超えは 0.1–2.0%（Linux）/ 5.0%
  （macOS VM）。超過は起動直後、一覧のページ切替（50 行の差し替え）、PDF を開いた直後、キー連打中の
  詳細ペイン更新に集中する。macOS VM の一覧ホイールは app p95 10.2 ms で 120 Hz 予算を超える
  （仮想 CPU・Retina 相当の 2x 描画の影響か未切り分け）。
- **Linux の present 込みの値は予算を常に超える。** GPUI の draw+present は全フレームで 16.7 ms
  超（一覧 p50 20–37 ms、Expand 後の大きなページ画像を含むフェーズ p50 36–87 ms・p95 38–131 ms）。
  CPU で Vulkan を実装する lavapipe がラスタライズを担うためで、GPU のある実機の値ではない。
  同じ操作の macOS VM（Metal）では draw+present が一覧ホイール p50 5.9 / p95 11.2 ms、ページ送り
  p95 11–13 ms（60 Hz 予算内、120 Hz 予算は p95 で超過）。present の評価は実機（§16.1）で行う必要がある。
- **ページ送り**: 読む速さ（700 ms 間隔）の隣接ページ送りは prefetch 済みのため Linux p95 16–18 ms
  （入力から 1–2 フレーム）、macOS VM p95 4–11 ms。キャッシュ外のページへのジャンプは Linux
  p50 187–304 ms・p95 205–428 ms で、ほぼ PDFium の描画（papersd）と RGBA の JSON/base64 転送の
  時間（GUI 側のフレームは p95 2.6–6.8 ms）。#85 の転送方式変更の効果はこの `page_jump` で比較できる。
- **PDF 初回表示**: Linux でビューア生成→初回 paint 146–215 ms、入力（Return）から 201–344 ms
  （paper 読み込み IPC と 1 ページ目の描画を含む）。
- GPUI の draw+present の行数はアプリ側のフレーム数より少ない。GPUI はキー入力の dispatch 前に
  同期的に draw することがあり（`Window::dispatch_key_event`）、その描画は計測対象の frame
  callback を通らない（present は次の callback で行われる）。そのため draw+present はフェーズ単位
  の分布としてだけ使い、フレーム単位では対応付けない。

## 未測定・制限

1. **実機での値**: 上記のとおり VM・ソフトウェア描画のみ。Apple Silicon 16 GB 実機、実 GPU の
   Linux、120 Hz（ProMotion）ディスプレイでは未測定。120 Hz で実際に描画できているかは
   ディスプレイ側のリフレッシュレートにも依存し、ここでは判定できない。
2. **present・コンポジタの遅延**: アプリのフックは present を含まない。present 込みは GPUI の
   `ZED_MEASUREMENTS`（上流の内部計測、将来変わりうる）を使った分布のみで、表示までの遅延
   （入力→光子）は測っていない。
3. **タブ切り替え**: M6-11（PDF ワークスペース）が未実装のため対象がない。M6-11 の後に span を
   追加する（`frame_timing::Span` に種類を足し、切替操作で `begin`、表示完了で終了）。
4. **1 万件の連続スクロール**: 現在の一覧は 50 件ずつの keyset paging で、1 万件を 1 本の
   リストとしてスクロールする UI はない。測ったのは 1 ページ内のホイール/キー移動とページ切替。
   #84 で一覧が変わったら同じハーネスで再測定する。
5. **macOS の PDF 系フェーズ**: Quartz イベントで一覧（ホイール・キー・ページ切替）と `J`/`K` の
   ページ送りは測れたが、次は測れていない。
   - Return キーが GUI に届かない（パレットの実行もページ欄の送信もされない）。PDF はパレットの
     1 行目のクリックで開いたが、`page_jump` は未観測。
   - PDF 初回表示が 50.2 s かかった（クリック後 0.29 s のフレームの次のフレームが 50 s 後）。
     macOS 初回描画時の PDFium のフォント列挙（非埋め込み日本語フォント）、またはフォーカスのない
     プロセスのタイマー抑制（App Nap 等）が候補だが切り分けていない。代表値として扱わない。
   - `pdf_wheel` ではスクロールが発生しなかった（座標またはスクロールイベントの単位の問題と推測）。
   - `pdf_keys` の `j`/`k` がページジャンプとして処理された（`page_turn` span が 114 件）。直前の
     `J`/`K` で付けた Shift フラグが HID の修飾キー状態に残ったと推測。無効として除外する。
   macOS の app 側フレーム（一覧・`J`/`K`）は有効だが、VM・1024x768 仮想ディスプレイ・
   ウィンドウ縮小のため実機の値ではない。
6. **ばらつき**: ローカルは他 agent と共有するホストで、同条件 2 回でも present 側の p95 が
   最大 1.5 倍程度ぶれた。各フェーズのサンプル数も少ない（ページジャンプ 8、初回表示 1）。
   傾向の把握用であり、回帰判定の閾値には使わない。
