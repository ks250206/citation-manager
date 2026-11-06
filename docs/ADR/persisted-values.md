# #7: 永続 ID、provenance、ページ参照

状態: 採用（最小 value contract）。DB migration は #8、具体的な annotation は #19。

- Paper/Attachment/Collection/Tag の内部 UUID は互いに異なる型とし、外部 ID を主キーにしない。
  UUID の生成は永続化を所有する側で行う。現段階では UUIDv7 生成ポリシーを実装しない。
- `ExternalId { namespace, value }` を Paper に保持し、DOI/arXiv/PMID などを保存する。
  `namespace` ごとの正規化・複数版の扱い・重複照合は #33（format/provider adapters は #13/#14） が所有する。
  DB は外部 ID を専用関連表等に保持する。未知 namespace を捨てず、外部 ID を内部 ID に代用しない。
- `AnnotationSource::Ai` は `ai` として保存する。human/import と区別し、後から確認・取消可能にする。
  source は由来であり、認可や内容の正しさを証明しない。
- `Anchor::Unresolved` は attachment、quote、reason を保存し、解決できなかった位置を失わない。
  ページを推測して Resolved にしない。PDF 差し替え後は再解決と確認が必要。
  保存側は source と Anchor を両方保持する。#7 は全 annotation schema や幾何型を固定しない。
- `PageIndex` は 0-based の物理 index。永続化・検索結果・引用追跡はこれを使う。
  `PhysicalPageNumber` は 1-based の物理表示番号。`page_index + 1` で相互変換する。
  index 最大値は u32::MAX-1、表示番号は 1..=u32::MAX。construct/deserialize の両方で検証する。
  実 PDF の page count に対する範囲検査は PDF/service 側が別途行う。
- `page_label` は PDF のラベル（`iv`, `S12`, `1` 等）であり任意の文字列。物理番号から生成しない。
  ラベルがなくても index を追跡できる。ラベルを計算やジャンプの index に使わない。
- Attachment は PaperId、managed relative StorageKey、SHA-256、元の名前、MIME、byte size を持つ。
  StorageKey は絶対パス/親参照/空成分等を拒否するが、symlink containment・atomic 書込・
  実 hash 検証は #9 の file-store 実装が行う。ユーザーの原本を変更しない。

DESIGN §6.3 の SQL は概念抜粋。external IDs / source / unresolved / label を落とした
完成 schema と解釈しない。domain の serde は保存境界の最小契約であり、公開 IPC schema は #25。
