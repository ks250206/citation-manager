# #7: daemon の信頼境界

状態: 採用方針。#25 で最初の IPC から認証/authorizer を実装・自動試験した（[daemon-ipc ADR](daemon-ipc.md)、
[Linux 証跡](../validation/ISSUE_25.md)）。macOS の peer/.app 起動、永続 token 管理 UI は #34/#26。

## 権限を与える主体

`papersd` が認証済み credential を検証し、daemon が所有する永続 grant を解決する。
grant は user-approved capability と対象リソース、失効/期限を含み、操作ごとに daemon 側で
**default-deny** 判定する。認証不明、未付与操作、失効 grant、grant 所有者が違う preview は拒否する。
transport adapter が認可済みと主張しても代用しない。read も認証対象であり、認証/認可前の
ライブラリ読み出しや副作用を禁止する。#25 の最初の IPC を一時的に無認可で提供しない。

client kind、要求 scope、UID、PID、実行名は診断/要求入力にすぎず、権限の根拠ではない。
特に `client_kind=desktop` や自称 `scopes=[write]` から grant を合成しない。
credential の発行・ユーザー同意・配布・保管・失効/rotation と接続への binding は #25/#26 が具体化する。
Keychain/保護された credential store と token hash 保存を候補とし、秘密や本文をログに出さない。
この ADR は署名済み credential や安全な配布の実装完了を意味しない。

## 最小権限

- Native host の grant は capture/preview、#22 の preview 束縛 capture commit（`CaptureCommit`）と、
  保存先選択に必要な最小 collection/tag 名・ID の読取のみ。
  任意の Paper/PDF 全文読取、任意パス、一般的な更新/削除/commit の権限を与えない。
  capture の保存確定は user-approved preview と許可された destination に結び付く限定操作として
  daemon が検証する。preview handle は credential ではない。具体的な確認経路と operation は #25 と
  browser capture の Issue で実装する。
- MCP は read-only を既定にする。最初にユーザーが許可した読取 grant の範囲のみ。
  未ペアリングは read も拒否する。write は独立した明示的ユーザー許可と別 grant/capability を要し、
  MCP 接続や `source=ai` を write 許可と解釈しない。
- GUI/CLI も daemon で同じ検証を通る。storage crate を直接開く抜け道を作らない。
- HTTP は既定無効。loopback、Host/Origin、CORS default-off、認証、上限/timeout を #27 で実装する。
- Browser allowlist/origin/サイズ検査は入口の追加防御であり、daemon 認可の代替にならない。
  URL/HTML/PDF/外部 ID/ファイル名は信頼しない入力。SSRF・path traversal・resource exhaustion の
  制限は担当 adapter と daemon が行う。

## 対象と限界

悪意ある Web ページ、無権限の拡張、未認証ローカルクライアント、scope 偽装や grant 失効後の再利用を
対象とする。OS user 単位のファイル権限や socket peer credential は追加防御として使える。
ただし同一 UID の悪意ある任意プロセスがユーザーファイル・プロセスメモリ・認証 material にアクセスできる
状況を完全隔離できるとは主張しない。PID/コード署名の観測だけでこれを解決したと扱わない。
強い隔離には sandbox/entitlements/OS protection と credential 保管の追加検証が必要。
PDFium は native parser であり、worker 直列化だけで sandbox にならない。

#25 の必須テスト: 未認証/未知/失効/不足 grant の拒否、client kind/scope/UID/PID 偽装の無効性、
native host の読取制限、preview の grant binding、MCP read-only と write の独立許可。
これらは #7 で実行済みとしない。

## #22 実装状態

`CaptureCommit` capability と `CaptureSubmit/CaptureSave/CaptureCancel/CapturePdfStatus/CapturePdfRetry`
operation を追加した。capture preview は作成 grant に束縛し、LibraryWrite なしでは Create/Skip のみ
（Merge は `Forbidden(ImportCommit)`）、PDF は preview が提示した候補の index でのみ選べる。
`capture.pdf_fetch` は `JobCommand::Submit` では作れない。PDF 取得は SSRF 対策付き fetch policy
（HTTPS、全 DNS 応答と redirect の公開 unicast 検査、接続先 pin、cookie/credential なし、サイズ/時間/MIME/
`%PDF-` 検査）を通る。詳細は [browser capture ADR](browser-capture.md)。

## #25 実装状態

`cm_core::policy` が capability/operation 表と default-deny authorizer を持ち、`papersd` が全 command を
現在の保存 grant で判定する。credential は 256bit 乱数で SHA-256 のみ保存。client kind/要求 scope は
narrowing のみ。native host / MCP read-only preset、GrantAdmin 経由の別 grant による write 許可、
失効/期限の live session への即時適用、preview の grant binding を自動試験した。
owner の bootstrap credential は library 内 0600 file であり、同一 UID の任意プロセスからの保護は主張しない。
