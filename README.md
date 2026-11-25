<img src="assets/icon.svg" width="64" height="64" alt="">

# citation-manager

Created in [T3 Code](https://t3.codes).

Rust + GPUI のローカル文献管理アプリを実装中です。#7 の 12-crate workspace と最小契約に、
#8 の SQLite library / 文献 CRUD を追加しました（[利用API・保存境界](docs/ADR/sqlite-library.md)、
[実DB検証](docs/validation/ISSUE_8.md)）。
#10 では macOS の実 GPUI window と手動登録/編集/一覧/詳細を追加しました
（[起動・実機検証](docs/validation/ISSUE_10.md)）。#15 は実 library query による分類・metadata 検索・
ページング、書誌 preview/import/export、PDF 添付/新規登録、private 設定を接続しています。
**#15 の native CUA acceptance は未完了**です（[検証・blocker](docs/validation/ISSUE_15.md)、
[保存と worker 境界](docs/ADR/library-workbench.md)）。#25 で library ごとの `papersd`（唯一の writer、
認可付きローカル IPC、永続 job）を追加し、GUI は IPC 経由で使います（[daemon-ipc ADR](docs/ADR/daemon-ipc.md)、
[Linux 検証](docs/validation/ISSUE_25.md)）。#28 の CLI `papers` は GUI なしで同じ daemon を遅延起動して使えます
（[CLI ADR](docs/ADR/cli.md)、[検証](docs/validation/ISSUE_28.md)）。`papers mcp` で同じ library の MCP stdio サーバーも起動できます。
REST（#27）は API 設定で有効化します（GUI の API & integrations、または `papersd api set --enable`）。
#11 の PDFium runtime は [再現可能な検証PoC](docs/validation/PDFIUM_POC.md) を追加し、
専用worker・継承CropBox/回転/Unicode・注釈搬出をPDFKitと独立照合しました。製品viewer/exportは後続です。
#29 で既定 read-only の MCP stdio サーバー `papers-mcp`（papersd IPC 経由、12 tools、出典付き結果）を追加しました（[mcp-server ADR](docs/ADR/mcp-server.md)）。書き込みは所有者が別途承認した grant が必要です。

設計は [DESIGN.md](DESIGN.md)、依存方向と信頼境界は [ADR](docs/ADR/boundaries.md) /
[security](docs/ADR/security.md)、変更ルールは [CONTRIBUTING](docs/CONTRIBUTING.md) を参照してください。

Rust は `rust-toolchain.toml` の **1.99.1**、依存解決は `Cargo.lock` に固定しています。
Python 3.11+ が検査スクリプトに必要です（CI/初期検証は 3.14）。

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/check_architecture.py
python3 scripts/check_migrations.py --base origin/main
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

通常の lane は GPUI/PDFium をコンパイル・ロードしません。SQLite は bundled C source をビルドします。
optional dependency の解決/lock 生成にはネットワークが必要です。
macOS + Xcode（初回起動/ライセンス同意済み）で native dependency lane を追加確認できます。
Metal compiler が使えるか確認し、不足時のみ追加取得します。
Xcode 16 の同梱 compiler が使える場合、追加取得は不要です。

```sh
if ! xcrun --sdk macosx metal -v; then
  xcodebuild -downloadComponent MetalToolchain
fi
xcrun --sdk macosx metal -v
cargo check -p cm-desktop --features native-gpui --locked
cargo clippy -p cm-desktop --all-targets --features native-gpui --locked -- -D warnings
cargo build -p cm-desktop --features native-gpui --locked
cargo check -p cm-pdf --features bindings-check --locked
```

この lane は macOS backend/Metal shader と Rust bindings のコンパイル確認です。
ウィンドウ表示・PDFium dylib のロード/ABI・コード署名の成功は意味しません。
実行結果、実機担当と未実施 blocker は [TEST_MATRIX](docs/validation/TEST_MATRIX.md) に記録します。

#10 の実行ファイル（macOS、未署名のローカル開発用。Linux は下記）:

```sh
cargo build -p cm-daemon --bins --features desktop-bootstrap --locked
# 専用絶対パスを明示する。検証では helper が作った temp library のみを使用。
python3 scripts/prepare_native_smoke.py --deny-network
# 表示された open コマンドを実行。再起動も同じコマンド/同じ library を使う。
```

#### Linux（x86_64、X11/Wayland）

同じ native lane を Linux でもビルド・実行できます（GPUI の X11/Wayland backend、Vulkan 対応 GPU/ドライバが必要）。
Ubuntu 24.04 の例:

```sh
sudo apt-get install -y libxkbcommon-dev libxkbcommon-x11-dev libx11-xcb-dev libxcb1-dev \
  libwayland-dev libfontconfig-dev libfreetype-dev libvulkan-dev mesa-vulkan-drivers
# citation-manager は同じディレクトリの papersd を必要に応じて起動する（#25）。両方をビルドする。
cargo build -p cm-daemon --bins --features desktop-bootstrap --release --locked
# ~/.local/bin と ~/.local/lib/citation-manager/libpdfium.so（chromium/8125, checksum 固定）に配置し、
# デスクトップエントリを作成。PDFium の取得にはネットワークが必要（--skip-pdfium で省略可）。
python3 scripts/install_linux.py
~/.local/bin/citation-manager --library ~/.local/share/citation-manager/library \
  --settings ~/.config/citation-manager/ui.json
```

Linux では ⌘ の代わりに Ctrl を使います（Ctrl+N / Ctrl+S / Ctrl+F / Ctrl+Q など）。
ファイル選択ダイアログは XDG Desktop Portal を使うため、portal が動いているデスクトップ環境が必要です。

⌘N で新規、Info の metadata を入力し ⌘S で共通 preview を開き、明示的な decisions を確認して保存します。
保存済み文献はテーブルから選んで編集し、⌘S で更新。⌘F で metadata 検索欄へ移動し、↩ で検索します。
テーブルでは ↑/↓ で選択移動、↩ で Title 欄へ移動、Esc で入力欄からテーブルへ戻ります。
⌘1 Library、⌘2 Import review、⌘, Organize & export。未保存の編集は下部ステータスバーに表示されます。
⌘Q で完全終了し、同じ library/settings で再起動します。
既定の実ライブラリは自動接続しません。HTTP は既定で無効です（API & integrations で有効化すると
`papersd` が 127.0.0.1/::1 の REST API を提供: [rest-api ADR](docs/ADR/rest-api.md)、[OpenAPI](docs/openapi.json)）。GUI は library ごとの `papersd` を
同じディレクトリから遅延起動し、0700 の runtime directory 内 0600 Unix socket と library 内の
bootstrap credential（0600）で接続します。GUI 終了後、client と active job が無ければ daemon は idle 終了します。

ブラウザ保存（#23）: `papers-native-host` を `papersd` と同じディレクトリに置き、ブラウザ用 manifest を登録する。
正式な拡張（#24）の ID が決まるまでは開発用 smoke 拡張の ID のみ（明示時）。詳細は
[native messaging ADR](docs/ADR/native-messaging.md)。

```sh
cargo build -p cm-daemon --bins --features desktop-bootstrap --release --locked
cargo build -p cm-native-host --release --locked
python3 scripts/install_linux.py --development-extensions --pair-native-host
~/.local/bin/papers-native-host status   # 移動/更新後は: papers-native-host refresh
```

```sh
# GUI なしで daemon だけを起動する場合（通常は client が自動起動する）
target/debug/papersd --library /absolute/private/library [--idle-timeout-ms 30000]
```
現在の追加経路は新規 metadata の preview、DOI（read-only）、UTF-8 BibTeX/RIS、PDF files/folder。
PDF 既存添付と新規文献化は別操作です。Info/Notes/PDF host のうち notes 編集と PDF viewer はまだ利用できません。
各書誌 batch は最大100件の別承認が必要で、途中 cancel は以前の commit を取り消しません。
設定の既定保存先は専用 library 内 `ui-settings.json`、明示 absolute `--settings` でも指定できます。
複数行入力/Undo/redo、未保存変更の移動確認と終了中 write の結果照合には制限が残ります。

## ライセンス

MIT License（[LICENSE](LICENSE)）。同梱する PDFium・GPUI などのサードパーティのライセンスは
[docs/licenses](docs/licenses) と [pdfium-licenses](docs/ADR/pdfium-licenses) を参照してください。
