# #11 PDFium runtime / annotation portability PoC

状態: **この corpus の macOS 実行・独立照合を実測済み**。compile-only 成功と分離する。
設計判断/制限は [ADR](../ADR/pdfium-runtime.md)、固定物は
[runtime-lock](../../scripts/pdfium/runtime-lock.json)、[corpus/許諾](../../fixtures/pdf/README.md)。

## 再現

macOS + Xcode command-line tools（PDFKit/Swift）+ Rust 1.99.1 + Python 3.14。
作業用 directory は Git 無視の `target/`。バイナリ自体はコミットしない。

```sh
uv venv target/pdfium-tools
uv pip install --python target/pdfium-tools/bin/python -r scripts/pdfium/requirements.txt
# または python3 -m venv + venv 内 python -m pip install -r ...
python3 scripts/pdfium/fetch.py
cargo clippy -p cm-pdf --all-targets --features runtime --locked -- -D warnings
target/pdfium-tools/bin/python scripts/pdfium/run.py --out target/pdfium-validation/reproduction
```

`--out` は**存在しない directory**を指定。再実行は別名を使う。fetch は固定 SHA-256 を照合する。
runner は explicit runtime tests、PoC export、export-only appearance completion、最終 PDF の PDFium 再読込、
PDFKit による原本/native/portable の独立 render/文字・注釈抽出、数値照合、packaging inspection を順に実行。
macOS/PDFKit/dylib 不在・検査失敗は非zero exit。skip-as-success はない。

生成物（上記 directory 内）:

- `pdfium-report.json`: 12 回の render/extraction 時間・寸法・marker 誤差、全文字枠、標準注釈 quads、typed status。
- `pdfium-export.pdf`: native 保存の中間物（`/AP` 欠落、製品の完成 export として扱わない）。
- `portable-export.pdf`: pypdf による export-only `/AP` 補完。原本とは別ファイル、新規作成のみ。
- `original-*.ppm`, `pdfium-portable/page-*.ppm`: PDFium の raster。
- `pdfkit-{original,native,portable}/page-*.png`, `report.json`: Apple PDFKit の独立 raster、文字枠、注釈内容。
- `verification.json`: quads/文字枠/色/位置/appearance/original hash の定量照合。
- `packaging.txt`, `run.json`: machine/時刻、otool install_name/dependencies/minOS、ad-hoc signature 観測。

CI は既存 `contracts`（PDFiumなし）/`native-dependencies`（bindings compile-only）を保持し、
`pdfium-runtime` を追加。固定 universal artifact で実行し、`pdfium-runtime-evidence` artifact に上記証跡を保存。
CI の実行状態と異モデルレビューは PR 上で別途確認し、本ローカル結果から推測しない。

固定archiveのSigstore attestationも `gh attestation verify ... --repo bblanchon/pdfium-binaries --bundle ...`
で成功。公開証明書/subject/builder情報の [verification snapshot](PDFIUM_POC/attestation-verification.json) を保持。

## 今回の観測

Apple Silicon arm64、macOS 27.2 (26B123)、Rust 1.99.1、PDFium 158.0.8125.0。
[verification.json](PDFIUM_POC/verification.json)、[measurements.json](PDFIUM_POC/measurements.json)、
[packaging.txt](PDFIUM_POC/packaging.txt) は実行 snapshot。時間は debug build の小 corpus の一回測定で、
§16 の release/RSS/performance 目標を満たした意味ではない。raw evidence の worktree 内 path は
[snapshot run.json](PDFIUM_POC/run.json) に残す。外部 reviewer は上の runner で再生成する。

| 実測対象 | 結果 |
|---|---|
| 固定 dylib のロード | hash 一致・8125 required symbols bind・実 render/extraction 成功 |
| 4 rotations × zoom 0.5/1/2 | 12 bitmap、実 marker の centroid error 0 px |
| Unicode/複数行 | 日本語、αβ、café の抽出、4 pages × 42 non-space chars を PDFKit 文字枠と照合 |
| 複数行 highlight | 4 pages × 2 quads、PDFKit との差 最大 0.000014 pt |
| 文字枠 | loose box と PDFKit selection の差 最大1.512 pt。descender/overhang の metric 差、位置ずれとして放置していない |
| 色・位置 | text ink と2px縁を除いた期待矩形内で両rendererの黄色coverage 100%、範囲外の黄色なし |
| Unicode comment / note | 各4件、標準Contentsを再読込一致、noteは24pt iconとして独立描画 |
| appearance | native 8件に `/AP` なし。補完後 8件の `/AP /N` Form + 複数行polygonを構造と両rendererで検証 |
| 原本不変 | 全処理前後の SHA-256 一致。別PDFへ搬出 |
| labels/index | `iv`/`S12` を物理indexと混同しない、index 6/最大値を InvalidPage |
| 画像のみ | NoExtractableText、本文空。OCRせず閲覧は可能 |
| 破損/暗号化 | Malformed、password missing/wrong を typed error、正しいpasswordでUnicode再抽出 |
| cancellation/cleanup | 事前/投入後queued cancel、期限切れ、queue overflow、4 callers × 5 requests、失敗後の再開成功 |
| corpus 再生成 | 固定font・固定tools・font timestamp再計算OFFで3PDFがbyte-identical（[hash証跡](PDFIUM_POC/fixture-regeneration.json)） |

PDFKit と PDFium の text ordering は回転で変わる。全ての non-space 文字を**同じ文字と物理位置**で
対応付けて照合し、抽出順の同一性を要求しない。metric 比較の閾値は top/左右0.5pt、全bounds2pt。
実際の最大差1.512ptは `p` のdescender、overhang最大0.434pt。PDFKitの選択枠とink extentの定義差を
座標transformに補正値として混ぜない。page markerとannotation quadの検査は別で継続する。

## 検証コマンドと未完範囲

通常: `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、
`cargo test --workspace --locked`、`cargo build --workspace --locked`、architecture/migration (`--base origin/main`)、
Python validator 16 tests を実行。optional binding compile と runtime feature clippy/build/tests は別途実行。
依存追加は cm-pdf の serde（DTO）/serde_json（test/example）/sha2 0.10.9（dylib pin）だけ。
`cargo generate-lockfile` 後、無関係の cc 更新/依存選択を cargo update で戻し、lock差分はcm-pdfの3 edgesのみ。
workspaceの宣言済みPDFium ABI/featureやshared GPUI policyは変更していない。

未実施/残す事項: .appからの実load/移動(#34)、Developer ID/hardened runtime/notarization(#31)、
viewer(#17)、DB/storage接続(#8/#19)、製品のexport helper配送/実装(#20)、任意角度glyph/縦書き/ligature/2段組、
UserUnit≠1、署名付き/アクセス制限付きPDF、任意annotation編集、native hard timeout/sandbox/RSS上限。
M0のPoCはPython補完で搬出を実証したが、Python runtimeを製品同梱したとは主張しない。
