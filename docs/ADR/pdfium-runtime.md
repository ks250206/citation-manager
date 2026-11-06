# #11: PDFium ABI、serial worker、注釈搬出と配布

状態: **M0 PoC 採用**。#17 の viewer、#19/#20 の注釈保存/製品 export、#34 の .app 実ロード、
#31 の署名/notarization は別ゲート。実測は [PDFIUM_POC](../validation/PDFIUM_POC.md)。

## 固定した組合せと一次資料

- `pdfium-render = 0.9.5`, `default-features = false`, `pdfium_8125 + thread_safe` は #7 の選択を維持。
  [公式 source](https://github.com/ajrcarey/pdfium-render)、[0.9.5 API](https://docs.rs/pdfium-render/0.9.5/pdfium_render/)。
- [bblanchon release chromium/8125](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium/8125)
  の **PDFium 158.0.8125.0**, `pdfium-mac-univ.tgz` (arm64/x86_64, macOS 13+)。
  builder commit `4bdb7acd45b39ef6a7fc2a5b4c83f0398e45c9a6`。
  release/attestation が固定する builder とアーカイブの provenance であり、Chromium source 全体の
  再ビルド再現性をこの PoC で証明した意味ではない。
- 正式な URL、archive SHA-256、dylib SHA-256、binding features は
  [`runtime-lock.json`](../../scripts/pdfium/runtime-lock.json)。`latest` や system library の探索は禁止。
  fetch は hash 不一致を失敗とし、version と notices を確認する。
- 動的 loader は絶対パスの dylib hash を検証し、8125 binding の全 required symbols を解決する。
  PDFium 公開 API に library build-version 問合せはない。実行時 build の識別根拠は**実ロードした
  dylib の hash と固定 archive の VERSION**である。文書の PDF version と混同しない。
  hash読取とdlopenは別openなので、その間の差替えをatomicに防ぐものではない。
  信頼された開発cache/bundle pathを前提とし、署名identityを含むrelease trust policyは#31で検証する。
- archive の `args.gn`: V8=false、XFA=false。実ロード/描画/Unicode 文字枠/標準注釈保存は実証。
  JavaScript/XFA/OCR capability を提供しない。
- [8125 annotation header](https://pdfium.googlesource.com/pdfium/+/refs/heads/chromium/8125/public/fpdf_annot.h)
  と取得 archive の `include/fpdf_annot.h` を確認。SetColor/GetColor と既存 AP の制約も区別する。

## スレッドと制限

`PdfWorker::start` の専用 `cm-pdf-serial` thread 上で binding をロードし PDFium を構築する。
要求は所有済み path/password/index/quad/text、応答は所有済み文字列/幾何/bitmap/PDF bytes。
page/document/text/bitmap/annotation ハンドルを thread 間に渡さず、独自の unsafe Send はない。
各要求が document を開いて処理し、応答を送る前に全 document 関連 handle を drop する。
caller は UI thread で wait せず、既存サービスのジョブ実行から利用する。

upstream 0.9.5 は `OnceLock` に process-global binding を保持するため、**この adapter の worker は
1 process で一度のみ start** できる。shutdown 後も dylib/global native library は process 終了まで残る。
複数 worker、別 ABI への差替え、再初期化は `AlreadyStarted`。document cleanup と library unload を
混同しない。将来の daemon 再起動が library 更新の境界。`thread_safe` は保険であり、直列安全性の
実証の代替にしない。4 caller × 5 request と異常系の後の成功を runtime で検証した。

上限: queue 8、入力/保存後出力 32 MiB、render 4,194,304 pixels / 各辺8192、1 page 100,000 chars、
1 operation 1024 annotations、総4096 quads、総1 MiB comment/note（各64 KiB）。request budget は最大60秒。
キャンセル/期限は load の前後、文字ごと、render/save の前後、注釈ごとに検査。ticket drop/timeout も
cancel する。完了と cancel の競合では既に返された結果が勝つ。事前 cancel、投入後の queued cancel、
期限切れ、queue overflow を検証。**native 呼出し中の強制停止、総 native allocation/RSS の上限、
悪意ある PDF の sandbox は未実装**。save-to-bytes の native 内部 buffer は保存後にサイズ検査する。
shutdown の join も native call が戻るまで待つ。hard timeout/強制隔離には別 process が必要。

malformed、password missing/wrong、unsupported security、invalid index、image-only を typed status/error で返す。
password は Debug/log に出さない。PDF 層は Paper/storage を更新せず、書誌を消す機能を持たない。
画像のみは NoExtractableText として扱う。これは OCR やページに画像があることの一般判定ではない。

## 座標とページ

保存/adapter 座標は unrotated PDF user space、単位 point、bottom-left。有効表示領域は
MediaBox ∩ CropBox（CropBox absent なら MediaBox）。display はその CropBox を原点に、
clockwise page rotation 0/90/180/270 を一回適用、zoom は device scale を含む pixels/point。
TopLeft/BottomLeft display origin を指定し、逆変換は同じ Viewport を使う。
範囲外 hit は別ページへ丸めず None。DB 0-based PageIndex、physical display 1-based、PDF label は独立。

character の tight box は ink bounds、quad は axis-aligned loose bounds の Z-order
(TL,TR,BL,BR)。page の回転と任意に傾いた glyph の exact quad は別問題で、後者は未保証。
文字の読み順は native の順であり、90° fixture では行順が PDFKit と異なる。selection の照合は
文字内容と物理位置で行い、文字列 offset の一致を推測しない。ligature、縦書き、2段組、任意角度の
文字、UserUnit が1以外の PDF は今後の corpus/設計拡張が必要。

## Export-only 代替の決定

実測: PDFium 8125 + wrapper 0.9.5 で Highlight/複数 quads/Unicode Contents/Text note を作成・保存・
再読込できた。ただし **8 注釈の全てで `/AP` が欠落**。PDFKit は独自 appearance を合成して表示するが、
それを persistent appearance の成功と呼ばない。Rust の `NativeExport` は
`AppearanceStatus::RequiresExportCompletion` を明示して intermediate bytes を返す。

DESIGN §9.3 の代替許可を使い、**搬出に限定して pypdf 6.1.1** で native output の標準注釈へ
明示 `/AP /N` Form XObject を付ける PoC を採用。
[`complete_appearances.py`](../../scripts/pdfium/complete_appearances.py) は別ファイルを `xb` 作成し、
既存 AP や未対応 subtype、非 axis-aligned quad を拒否する。Highlight は黄 /CA=0.5、Multiply、
2 行の polygon。Text note は 24pt icon、Unicode は標準 Contents に残す（icon に本文を描かない）。
原本の hash は前後同じ。最終 PDF を PDFium と独立した Apple PDFKit の両方で再読込/描画し、
座標/内容/色/位置を定量照合した。PDFKit は native note icon を24ptへ正規化するため PoC も24ptに合わせた。

これは製品に Python runtime を同梱する決定ではない。#20 で export helper の配送/代替実装を決定し、
同じ gate を再現する。Rust native output 単体を完成済み export として UI/API に渡してはいけない。
既存注釈の任意編集、署名付き PDF の署名保持、アクセス制限付きPDFの搬出、全 annotation subtype、
任意フォントの FreeText appearance は未検証。pypdf は抽出/表示/文字位置処理の代替にしない。
現helperはページ内の全注釈を走査するPoC専用で、既存の/APなしHighlightも黄色/0.5へ変更し得る。
corpusは既存注釈を持たない。#20では搬出前の注釈identity/数などで今回作成した注釈を識別し、
既存注釈の色/opacity/APを保持する処理と回帰テストが必要。このhelperを一般PDFへそのまま接続しない。
[公式 pypdf API](https://pypdf.readthedocs.io/en/6.1.1/modules/PdfWriter.html) と固定版 source を確認。

独立系は [PDFKit quadrilateralPoints](https://developer.apple.com/documentation/pdfkit/pdfannotation/quadrilateralpoints)
の bound-origin-relative Z-order を絶対 PDF 座標に戻す。
[PDFPage thumbnail](https://developer.apple.com/documentation/pdfkit/pdfpage/thumbnail(of:for:)) と
selection bounds で描画/文字位置を取得。GUIクリックは不要。PDFKit の使用自体が unavailable なら gate 失敗。

## #34 / #31 への handoff

`bundled_library(current_exe)` → `Contents/Frameworks/libpdfium.dylib` の**絶対パス**。
空白/Unicodeを含む bundle 移動も CWD/PATH 依存なし。開発 PoC の `CM_PDFIUM_LIBRARY` は example/runner
だけが読む明示 override であり、製品の自動 fallback にしない。#34 はこの API と同じ固定 original dylib
を使い、実際の .app executable から load/render の証跡を残す。この PR の絶対パス test は .app 実ロードの代替でない。

実物 `otool` 観測: install_name は `./libpdfium.dylib`、両arch minOS=13.0、SDK=26.0。
依存は AppKit/CoreGraphics/CoreFoundation/Foundation と libSystem のみ。追加の同梱 dylib/resource はなし。
明示 dlopen は install_name/rpath を探索せず、未変更バイナリで成功した。リンク配布や install_name を変更する
なら #34 で `@rpath` / `@loader_path` を設計・再検証する。修正すると hash と既存署名は変わる。

取得物は upstream の **ad-hoc signature**、TeamIdentifierなし。Developer ID の署名ではない。
配布時は nested dylib → helper executable → app の順で自組織 identity により署名し、notices を
`Contents/Resources/licenses/pdfium/` に同梱。hardened runtime の library validation、entitlements、
notarization/stapling は #31 の責任。Developer ID identity=0 は unsigned runtime PoC の blocker ではない。
**再署名後の hash は原本と異なる**ため #31 は検証済み post-sign digest / code-signature identity による
release trust policy を追加する必要がある。現 loader は fail-closed の原本 hash gate のままなので、
再署名した配布物をそのままロードできるとは主張しない。

[`pdfium-licenses/`](pdfium-licenses/) に固定 archive の LICENSE と全18 third-party notice の法文を保持し、
末尾空白/EOFのみ整形する。manifest で取得時と整形後の両 hash を固定し、fetchでLICENSE/全noticeの取得時hashとファイル集合も照合する。binary 再配布には PDFium BSD 条件と各 third-party 条件が適用される。
FreeType の credit 要件等を含むため top-level MIT のみで済ませない。binary は Git に含めない。
Rust wrapper の同梱物は MIT OR Apache-2.0の[選択表記](pdfium-render-LICENSE.md)のみで、
実際の許諾本文/著作権表示が揃った製品noticeではない。#31/#34で公式配布物/権利者の情報から
選択したlicenseの実文と適切な著作権表示を収集・確認するTODOを残し、完了前に製品配送済みとしない。
font は corpus のみ（OFL添付）、pypdf/ReportLab/fontTools/Pillow は開発検証ツールで製品には同梱しない。
これを全製品の法務/署名確認完了と解釈しない。
