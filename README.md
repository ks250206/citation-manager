<img src="assets/icon.svg" width="64" height="64" alt="">

# citation-manager

Created in [T3 Code](https://t3.codes).

Rust + GPUI のローカル文献管理アプリを実装中です。#7 の 12-crate workspace と最小契約に、
#8 の SQLite library / 文献 CRUD を追加しました（[利用API・保存境界](docs/ADR/sqlite-library.md)、
[実DB検証](docs/validation/ISSUE_8.md)）。GUI window、daemon/CLI、MCP tools はまだ利用できません。
#11 の PDFium runtime は [再現可能な検証PoC](docs/validation/PDFIUM_POC.md) を追加し、
専用worker・継承CropBox/回転/Unicode・注釈搬出をPDFKitと独立照合しました。製品viewer/exportは後続です。
`cm_mcp::run_stdio()` は #29 未実装を明示するエラーを返し、MCP stdout を出力しません。

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
