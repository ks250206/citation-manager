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

- `pdfium-report.json`: 元 corpus の12 回の render/extraction 時間・寸法・marker 誤差、全文字枠、標準注釈 quads、typed status。
- `pdfium-export.pdf`: native 保存の中間物（`/AP` 欠落、製品の完成 export として扱わない）。
- `portable-export.pdf`: pypdf による export-only `/AP` 補完。原本とは別ファイル、新規作成のみ。
- `original-*.ppm`, `pdfium-portable/page-*.ppm`: PDFium の raster。
- `pdfkit-{original,native,portable}/page-*.png`, `report.json`: Apple PDFKit の独立 raster、文字枠、注釈内容。
- `verification.json`: quads/文字枠/色/位置/appearance/original hash の定量照合。
- `inherited/`: 継承2ページの6回 render、同名のnative/portable PDF・両rendererのraster/報告・verification。
- `inherited-structure.json`, `inherited-after.json`: 生page treeと実workerの境界/搬出回帰。
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
| 4 rotations × zoom 0.5/1/2 + 継承90°×2ページ×3倍率 | 18 bitmap、実 marker の centroid error 0 px |
| Unicode/複数行 | 日本語、αβ、café の抽出、元4 + 継承2 pages × 42 non-space chars を PDFKit 文字枠と照合 |
| 複数行 highlight | 元4 + 継承2 pages × 2 quads、PDFKit との差 最大 0.000014 pt |
| 文字枠 | loose box と PDFKit selection の差 最大1.512 pt。descender/overhang の metric 差、位置ずれとして放置していない |
| 色・位置 | text ink と2px縁を除いた期待矩形内で両rendererの黄色coverage 100%、範囲外の黄色なし |
| Unicode comment / note | 各6件、標準Contentsを再読込一致、noteは24pt iconとして独立描画 |
| appearance | native 12件に `/AP` なし。補完後 12件の `/AP /N` Form + 複数行polygonを構造と両rendererで検証 |
| 原本不変 | 全処理前後の SHA-256 一致。別PDFへ搬出 |
| labels/index | `iv`/`S12` を物理indexと混同しない、index 6/最大値を InvalidPage |
| 画像のみ | NoExtractableText、本文空。OCRせず閲覧は可能 |
| 破損/暗号化 | Malformed、password missing/wrong を typed error、正しいpasswordでUnicode再抽出 |
| cancellation/cleanup | 事前/投入後queued cancel、期限切れ、queue overflow、4 callers × 5 requests、失敗後の再開成功 |
| corpus 再生成 | 固定font・固定tools・font timestamp再計算OFFで4PDFとmanifestがbyte-identical（[hash証跡](PDFIUM_POC/fixture-regeneration.json)） |

PDFKit と PDFium の text ordering は回転で変わる。全ての non-space 文字を**同じ文字と物理位置**で
対応付けて照合し、抽出順の同一性を要求しない。metric 比較の閾値は top/左右0.5pt、全bounds2pt。
実際の最大差1.512ptは `p` のdescender、overhang最大0.434pt。PDFKitの選択枠とink extentの定義差を
座標transformに補正値として混ぜない。page markerとannotation quadの検査は別で継続する。

## 継承box P1の実回帰と修正

[独立レビュー](https://github.com/ks250206/citation-manager/pull/37#issuecomment-6681805837) の指摘を、
新規 `inherited.pdf` と固定dylibの実workerで再現した。
[regression.json](PDFIUM_POC/inherited/regression.json) に同一fixture/hashのbefore/afterを保持。
修正前のadapterはmerge commit `0280218`（元実装/P3 `1e8edd8`と同内容）、probeを追加してから
`cargo run -p cm-pdf --example inherited_boxes_regression --features runtime --locked -- before target/inherited-before.json`
を実行し、その後adapterだけを修正して `after` を実行した。`before` は最終コードで成功させるmodeではない。
現在のrunnerは常に `after` の期待をassertする。

| case | 修正前（実測） | 修正後（実測） |
|---|---|---|
| MediaBox+CropBox+Rotateを継承 | Page/内側Exportとも NativeFailure | crop [20,30,380,470]、R90、440×360、Export成功 |
| direct MediaBox、CropBox+Rotateを継承 | crop [0,0,400,500]、500×400、crop外quadもExport受理 | 同じ正しいcrop/R90/440×360、crop外quadをInvalidGeometry |

[structure.json](PDFIUM_POC/inherited/structure.json) は `PdfReader.pages` を使わず生の辞書で、
親の3属性、page 0の全属性欠如、page 1のMediaBoxだけの存在を検査した証跡。
平坦化・親CropBox改変を拒否する実PDFのPython3件もrunnerで実行する。
Page/Exportの共有 `bounding()` が viewport/render と highlight/note範囲検査へ渡る。
MediaBox内・CropBox外のquad/noteの拒否、hit test、zoom 0.5/1/2を実fixtureで確認した。

継承の期待値は生成時の固定box/回転/marker座標で、highlightも抽出geometryに依存しない固定矩形。
PDFKitの原本/native/portable全段階でcrop・回転・寸法・marker・文字を照合し、native/portableの
Unicode comment/noteとquadsも比較する。最終appearanceは両rendererで確認した。
[継承verification](PDFIUM_POC/inherited/verification.json) は2×42文字、quad誤差0pt、marker誤差0px、
黄色内側coverage100%、4 appearance Forms、原本hash不変を記録する。
独立PDFKitの丸いhighlight縁を避ける既存2px margin/98%判定は維持する。初回に高さ30/26ptの
固定quadでPDFKit内側coverage94.7%を検出し、元corpusに近い20pt高の固定矩形にして再実行した。
縁のraster形状同一を主張しない。

最新のraw evidenceは [run.json](PDFIUM_POC/run.json) の出力directory、全生成物hashは
[evidence-sha256.json](PDFIUM_POC/evidence-sha256.json)。
[継承measurements](PDFIUM_POC/inherited/measurements.json) と元corpusのsnapshotはこの修正後runから再生成した。
過去のupstream attestation snapshotは維持し、今回の実行で再検証したという主張にはしない。

## 検証コマンドと未完範囲

通常: `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`、
`cargo test --workspace --locked`、`cargo build --workspace --locked`、architecture/migration (`--base origin/main`)、
Python validator 17 tests（P3のnotice集合/hash拒否を含む）+ PDF tools laneの実page-tree 3 tests を実行。optional binding compile と runtime feature clippy/build/tests は別途実行。
依存追加は cm-pdf の serde（DTO）/serde_json（test/example）/sha2 0.10.9（dylib pin）だけ。
現行main `08cb717` と元PR/P3の通常merge後に `cargo generate-lockfile` で解決を確認。
無関係なcc/flate2更新とtempfileの依存選択変更は採用せず、merge済みlockに対する
`cargo update -p cm-pdf --offline` で現manifestを再解決した。mainとの差分はcm-pdfの3 edgesのみ。
#8のdomain/core/storageコードと2本のimmutable migration/manifestはmainから変更していない。
migration validatorは `--base 08cb717dd26b174bcada3f291b0cd466f39b2ea7` と `--base origin/main` で2本保全を確認。
workspace testsは30 unit/integration + 1 compile-fail doctest、runtimeは5 unit + 1実PDF統合（両corpus）を通過。
workspaceの宣言済みPDFium ABI/featureやshared GPUI policyは変更していない。

未実施/残す事項: .appからの実load/移動(#34)、Developer ID/hardened runtime/notarization(#31)、
viewer(#17)、PDF注釈のDB/storage接続(#19、SQLite基盤#8は統合済み)、製品のexport helper配送/実装(#20)、任意角度glyph/縦書き/ligature/2段組、
UserUnit≠1、署名付き/アクセス制限付きPDF、任意annotation編集、native hard timeout/sandbox/RSS上限。
M0のPoCはPython補完で搬出を実証したが、Python runtimeを製品同梱したとは主張しない。
