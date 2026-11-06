# #7: crate と依存境界

状態: 採用（最小契約）。機能 API は担当 Issue で互換的に拡張する。

`A → B` は A が B に依存できることを示す。表の辺は許可集合であり、全辺の利用義務ではない。

| crate | 許可する workspace 依存 | 責務 / 後続 |
|---|---|---|
| domain | なし | Paper/Attachment、typed UUID、ページ、provenance |
| core | domain | 最小 Repository / AttachmentStore / ImportService port |
| storage | core, domain | SQLite/migration (#8)、ファイル (#9) |
| search | core, domain | FTS (#18) |
| pdf | core, domain | PDFium、座標 (#11) |
| metadata | core, domain | 共通 import (#33)、書誌/DOI adapter (#13/#14) |
| protocol | core, domain | 機能別 wire DTO。IPC envelope/JobStatus は #25 |
| desktop | protocol | GPUI (#10)、command/keymap (#16) |
| daemon | protocol, core, storage, search, pdf, metadata | composition root、唯一の永続化操作主体、認可 (#25) |
| cli | protocol, mcp | 将来の papers executable (#28) |
| mcp | protocol | stdio 入口のみ、サーバー/tools は #29 |
| native-host | protocol | browser capture 境界 |

`domain/core/protocol` は GPUI、SQLite、PDFium、HTTP、MCP SDK に依存しない。
外部 adapter は core の port を実装し、core が具象 adapter を呼ぶ方向を禁止する。
GUI/CLI/MCP/native-host は storage を直接開かない。protocol は core の中立 Import DTO を
再公開するだけで、#7 では全操作 enum、IPC envelope、JobStatus、command ID を生成しない。
`cm-` は package prefix。12 crate はすべて library として始め、動かない daemon/CLI を
成功する executable として公開しない。

port は worker 側の小さな同期契約。UI のイベントスレッドで blocking I/O をしない。
接続プール、transaction、非同期実行、ページングは #8/#25/各機能 Issue で決める。
Import の preview は Paper/Attachment を永続化せず、正規化済み候補と警告、opaque handle を返す。
commit は handle と明示的 decisions を受け取り、保存された preview の所有 grant・期限・
変更有無・候補の範囲/重複を再検証してから atomic に書き込む。これらは port の要求であり、
#7 では service 実装も認可も存在しない。merge/attachment import などは担当 Issue で追加する。

`python3 scripts/check_architecture.py` は Cargo metadata の通常/dev/target 依存を検査する。
循環を作れない許可集合、12 member、純契約層への framework 混入、driver 所有、GPUI 固定 rev と
optional macOS feature を検査する。意味が変わる境界変更は先行契約 PR でレビューする。
