# #7: .app 配置と起動の草案

状態: **暫定**。#34 の配置/寿命 PoC で確定する。#31 の署名/配布判断と合わせて見直す。

```text
Citation Manager.app/Contents/
  Info.plist
  MacOS/citation-manager        # GPUI executable (#10)
  MacOS/papersd                 # on-demand daemon (#25/#34)
  MacOS/papers                  # CLI / MCP wiring (#28)
  MacOS/papers-native-host      # Native Messaging entry (#34/browser issues)
  Frameworks/libpdfium.dylib    # candidate; actual ABI/install_name/signing unverified
  Resources/                   # icon, assets, licenses, browser host manifest templates
```

製品名/bundle ID/実行名は暫定。PDFium を current directory や任意の PATH からロードしない。
bundle の絶対解決パスを使い、開発時の明示 override は配布時の探索から分離する。
ライブラリの install_name/rpath、Apple Silicon/x86_64、ABI と再配布 license、依存 dylib、
署名順序/hardened runtime/entitlements は #11/#34/#31 で検証する。
#7 は dylib をダウンロード/同梱/実行せず、bindings の compile と runtime 成功を区別する。

候補の起動方式は GUI/CLI/MCP/native host が同じ bundle 内の daemon を絶対パスで遅延起動する方式。
launchd の常駐登録は初期要件としない。user ごとの private IPC path と単一起動 lock、
起動競合時の待合せ/timeout/version mismatch、接続中・job 中の寿命延長、idle 終了、異常終了復旧を
#34/#25 で試す。MCP の stdout に起動ログを混ぜず、daemon 起動と stdio transport を分離する。
GUI 内の早期 PoC は許すが、外部クライアント用の無認可 Core 呼出しをリリースしない。

#34 の証跡: GUI を閉じた bundle から CLI/MCP/host が同一 daemon に到達、同時起動で一つだけ、
接続/job 中は存続、idle で終了、bundle 移動後も resource/PDFium/manifest path が解決できること。
これらと #10 の window は未実施。Developer ID identity は初期環境で 0 件 (#31 blocker)。
未署名 cargo build を署名/notarization 成功と呼ばない。
