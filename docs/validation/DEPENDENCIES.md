# #7 依存の確認（2026-11-06）

find-docs の Context7 flow を実行:

```sh
npx ctx7@latest library GPUI 'GPUI Rust macOS dependency setup toolchain git revision'
npx ctx7@latest docs /websites/rs_gpui 'GPUI macOS setup cargo git dependency Metal Xcode'
```

取得した [GPUI docs](https://docs.rs/gpui/latest/gpui/) は pre-1.0 と macOS Metal/Xcode 要件を確認するために使用。
公開版 docs だけでは現行 git の platform 分割を確認できないため、以下の公式一次ソースを追加確認した。

| 依存 | 固定値 / 判断 | 確認元 |
|---|---|---|
| Rust | 1.99.1, edition 2024 / resolver 3 | [公式リリース](https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/)、`rustup toolchain install 1.99.1 --profile minimal --component rustfmt --component clippy` |
| GPUI / gpui_platform | `4a6121bb0f10c6e8037b741b7dc0a4cd639ae289` | [gpui manifest](https://github.com/zed-industries/zed/blob/4a6121bb0f10c6e8037b741b7dc0a4cd639ae289/crates/gpui/Cargo.toml)、[platform manifest](https://github.com/zed-industries/zed/blob/4a6121bb0f10c6e8037b741b7dc0a4cd639ae289/crates/gpui_platform/Cargo.toml)、[constructor](https://github.com/zed-industries/zed/blob/4a6121bb0f10c6e8037b741b7dc0a4cd639ae289/crates/gpui_platform/src/gpui_platform.rs) |
| SQLite driver | rusqlite 0.41.1, default-features=false, bundled+backup | [公式 release](https://github.com/rusqlite/rusqlite/releases/tag/v0.41.1)、[README](https://github.com/rusqlite/rusqlite/tree/v0.41.1)、`cargo info rusqlite@0.41.1` と取得した同版 Cargo.toml |
| PDF bindings | pdfium-render 0.9.5, pdfium_8125+thread_safe, default-features=false | [同版 docs](https://docs.rs/pdfium-render/0.9.5/pdfium_render/)、[公式 source](https://github.com/ajrcarey/pdfium-render)、`cargo info pdfium-render@0.9.5` |
| MCP SDK | rmcp 3.9.0, server+transport-io, default-features=false | [公式 SDK](https://github.com/modelcontextprotocol/rust-sdk)、`cargo info rmcp@3.9.0`、[同版 docs](https://docs.rs/rmcp/3.9.0/rmcp/) |

SQLite を rusqlite に一本化し、SQLx は併用しない。同期操作は daemon の worker が所有する。
bundled により OS SQLite の差を避ける。実際の DB/WAL/migration は #8、file store/バックアップ運用は #9。
0.41.1 の default に含まれる wasm 向け FFI は不要なので無効にし、native libsqlite3-sys を使う。
FTS5 と backup の capability probe は storage のテストで確認した。これは製品 DB 実装ではない。

GPUI は default-features=false、gpui_platform は同一 rev + font-kit。
optional `native-gpui` は macOS の `gpui_macos` / `gpui_apple` を含む constructor を型チェックする。
shader の compile を省略する runtime_shaders feature は有効化していない。
初回は Metal Toolchain 不足で失敗し、`xcodebuild -downloadComponent MetalToolchain` による
Metal Toolchain 17F109 の取得後、同じ rev の check を再実行して成功した。
上流の git 推移依存も Cargo.lock の commit/checksum で固定する。更新は意図的な PR で行う。

PDF bindings の `pdfium_8125` は将来の ABI 検証候補を明示するための compile-time pin。
対応する dylib はまだ選択/同梱/ロードしていない。thread_safe feature を有効にしただけでは
製品の worker 直列化・sandbox・安全な runtime 動作の証明にならない（#11）。

既知 workspace dependency は公式 Cargo registry の `cargo search NAME --limit 1` と各版の
`cargo info NAME@VERSION` で取得可否・repository・feature を確認し、完全一致指定で宣言した:

| 宣言 | version | 公式 docs |
|---|---|---|
| serde | 1.0.229 | https://docs.rs/serde/1.0.229/serde/ |
| serde_json | 1.0.154 | https://docs.rs/serde_json/1.0.154/serde_json/ |
| uuid | 1.29.0 | https://docs.rs/uuid/1.29.0/uuid/ |
| tokio | 1.54.1 | https://docs.rs/tokio/1.54.1/tokio/ |
| axum | 0.8.9 | https://docs.rs/axum/0.8.9/axum/ |
| clap | 4.6.7 | https://docs.rs/clap/4.6.7/clap/ |
| reqwest | 0.13.5 | https://docs.rs/reqwest/0.13.5/reqwest/ |
| tracing | 0.1.44 | https://docs.rs/tracing/0.1.44/tracing/ |

serde/serde_json/uuid は現在の契約・テストで利用する。他の既知依存（rmcp含む）は後続実装のための
workspace 宣言であり、まだ package が利用しないものを Cargo.lock が解決・コンパイルしたとは主張しない。
#29 等で利用を追加した時点で lock 再生成と該当機能の検証を行う。
この文書は「最新版」の継続保証ではなく、#7 着手時の確認記録である。
