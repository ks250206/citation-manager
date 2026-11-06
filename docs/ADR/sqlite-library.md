# #8: SQLite library と手動文献 CRUD

状態: 採用。#7 の最小契約に互換的に追加する。既存の PDF/worker/geometry 契約は変更しない。

## 保存先と信頼境界

`LibraryDirectory::default_macos_path()` は
`$HOME/Library/Application Support/Citation Manager` を計算するだけで、open/create しない。
呼出側が選んだ専用絶対パスを `LibraryDirectory::prepare(path)` に渡す。
テストは canonical な temp directory 配下のみを使い、ユーザーの実ライブラリには接続しない。

Unix の所有 UID は effective UID と照合する。専用 directory は正確に `0700`、DB と
既存 WAL/SHM/rollback-journal は正確に `0600`、regular file・single hard link を要求する。
既存の緩い権限を chmod で修復したり、既存ファイルを削除したりしない。不一致なら停止する。
作成は directory `0700`、DB は exclusive create `0600`。既存 directory にも毎回検査を行う。
相対パス、`.`/`..`、全 ancestor と DB/sidecar の symlink、file の hardlink、非通常ファイルを拒否する。
ancestor は root または利用者所有、group/other writable 不可（root 所有 sticky temp directory のみ例外）。
SQLite は URI mode/CREATE flag を使わず、`SQLITE_OPEN_NOFOLLOW` で検査済みファイルを開く。
SQLite 接続前後にも sidecar と directory を検査する。

これは OS の POSIX 所有権/permission による境界であり、同一 UID の悪意あるプロセスや root、
独自 ACL による追加権限、OS sandbox に対する完全隔離を主張しない。OS ACL/volume 設定は信頼する。
Unix 以外の disk library は権限保証を弱めず UnsupportedPlatform で停止する。
SQLite WAL の前提に従い、保存先は同一ホストの local filesystem とする。
単一起動ロック/daemon の single writer lifecycle は #25 が実装する。

## 接続・schema・migration

- driver は共有宣言済み `rusqlite = 0.41.1` bundled。全 factory 接続で foreign_keys=ON、
  file DB は journal_mode=WAL、synchronous=FULL、busy timeout 5秒。適用値も確認する。
  in-memory は SQLite の制約上 journal_mode=memory とし、foreign_keys と同じ schema を使う。
- immutable な `20261106T090000Z_issue-8_001_papers.sql` と `..._002_relations.sql` を追加した。
  manifest の SHA-256 と embedded SQL を実行時にも照合する。registry への登録点は
  `crates/storage/src/migrations/mod.rs`。後続 Issue は timestamp が既存より後の追加 SQL と
  manifest/embedded registry を一緒に追記する。適用済み SQL/ID は変更しない。
- `schema_migrations(version, checksum, applied_at)` が正本。version は immutable な完全ファイル名。
  recorded history は registry の厳密な prefix でなければならない。
  checksum 不一致、未認識/未来/欠落/順序違い、未version管理の非空DBでは停止し、推測して修復しない。
  履歴tableが存在しても0行なら、他のuser objectがないことをbootstrap前とwriter transaction内で確認する。
  SQLite内部名の除外にはliteralな `sqlite_` prefixを使い、似た名前のuser tableを見逃さない。
- pending SQL 全体と history の更新を単一の IMMEDIATE transaction にまとめる。
  最初の history table 作成も同じ transaction。DDL/DML/後続SQLのどこかで失敗すれば全体を戻す。
  SQL は検証・版管理された application resource であり、ユーザー入力 SQL を受け付けない。
- 初期 relational schema は papers、people、paper_people（role と全体の sort_order）、
  paper_external_ids（未知 namespace と順序）、collections/所属、tags/所属、attachments、
  fulltext_pages、annotations（source/anchor_state/unresolved_reason/page_label）、jobs、api_tokens。
  JSON/FK/unique/page range/annotation state を SQL 制約でも検査する。
  FTS index/trigger は検索実装 #18 が追加migrationで所有する。token の発行/認可、job実行、
  collection操作、annotation編集などのサービスが動いたという意味ではない。
  独立ノートの引用なしは `quote_text=''` で表し、引用を捏造しない。位置/編集serviceは#19で実装する。

## #9 へ渡すバックアップ境界

既存の versioned DB に pending migration がある場合、通常の `LibraryDb::open` は
`StorageError::BackupRequired` で停止する。
`open_with_backup(directory, &mut dyn MigrationBackup)` の trusted hook が成功した後にだけ更新する。
空の新規DBには失う既存データがないため、初期schemaのbootstrapにはhookを要求しない。
`DESIGN §7` のバックアップ実装は #9 が担当し、この境界が既存データを持つ upgrade の前提となる。

hook には元の SQLite connection と pending ID 一覧を渡す。source は read-only として扱い、
SQLite online backup API を使うこと。live WAL の `library.sqlite` だけをコピーしてはいけない。
source の write transaction 内では online backup ができないため、hook は transaction の前で呼ぶ。
その後 IMMEDIATE transaction 内で履歴を再照合し、別 migrator が履歴を変えていれば停止する。
呼出側は hook 開始から migration 終了まで文献書込を止める必要がある（daemon lock は #25）。
この hook 自体はファイルバックアップ、PDF保存、復旧、ローテーションを実装しない。
テストの memory snapshot は境界の実動確認であり、製品バックアップではない。

## metadata と更新規則

通常作成は storage が `Uuid::now_v7()` で internal Paper/Person ID を生成する。
外部 ID を UUID に変換しない。UUID は process 内で生成順、一覧は UUID のバイト表現と同順の
canonical text 昇順とする。異なる process/過去の explicit-ID 登録にまたがる厳密な時系列保証ではない。
timestamp は SQLite が UTC RFC3339 相当の millisecond precision で生成する。
created_at は更新時も元値、updated_at は更新時刻、deleted_at は soft delete の時刻を保持する。

Paper の追加項目は `Bibliography` を serde flatten/default する。既存 #7 fixture の入出力を変更せず、
追加の全書誌項目は SQL column/関連表に保存する。type 未指定は journal-article、extra_json 未指定は
JSON object `{}` に正規化する。任意 JSON 値は JSON value として保持し、元の空白/キー順は契約にしない。
publication_date は部分日付も含めた元文字列を保持する（calendar/provider 解釈は #33）。
Contributor は given/family/literal/ORCID/role を持ち、複数 role が交互に現れる順序も保持する。

DOI 正規化は `cm_core::papers::normalize_doi` に集約する。前後空白、doi: と doi.org/dx.doi.org の
http/https prefix（wwwを含む）とschemeなしのdoi.org/www.doi.org/dx.doi.orgを除き、ASCII小文字にする。
10. + 非空の数字列（`.`で区切るsubdivisionを許可）+ `/` + 非空suffixを検査する。
[DOI Handbook §3.3.2](https://www.doi.org/doi-handbook/html/)の数字列/subdivisionに合わせ、登録者codeの固定桁数を仮定しない。
URLのpercent-decodeはraw DOI suffixを誤って変更しないよう、#33/#14のURL入力adapterで一度だけ行う方針を確定する。
このraw normalizerはpercent escapeをdecodeせず、encoded slashしかないURLは未解析として拒否する。
Bibliography.doi と namespace=doi（大小文字無視）の external ID が複数あれば一致を要求する。
DB の doi_normalized に UNIQUE、citation_key に case-sensitive UNIQUE を置く。
外部IDは namespace/value を原文と順序のまま別表に保存する。未知 namespace も保持し、
DOI以外の provider照合/正規化/自動マージ/類似度重複判定は #33 で実装する。
同一文献内の完全一致 namespace/value 重複は保存前にInvalidInputとして拒否するが、他文献との非DOI外部ID一致は自動統合しない。
soft-deleted record の DOI/引用キーも予約を維持し、復元時の衝突を避ける。

作成は insert-only。DOI/引用キー/内部IDの衝突で既存文献を自動上書きしない。
`PaperRepository::save` の SQLite adapter も明示internal UUIDによる insert-only とし、upsertしない。
通常の呼出側は generated-ID の `PaperService::create(NewPaper)` を使う。
`update(&Paper)` は読み出した完全な書誌情報・著者・外部IDの明示的な置換で、patch/mergeではない。
編集しない項目は元の Paper に保持する。未変更の著者は person ID も維持する。
著者列を明示的に変更した場合だけ関連を置換し、その文献の孤立した旧personだけを削除する。
他の文献のperson、添付、collection/tag所属、注釈、本文は変更しない。
paper/author/external-ID の保存は一つの transaction。途中の UNIQUE/FK エラーでも全体を戻す。

通常の get/list と attachment読取は deleted paper を除外する。
trash は `PaperVisibility::Deleted`、管理読取は `get_including_deleted`、復元は明示 `restore` を使う。
soft delete/restore は同一状態なら冪等で、関連データを消さない。trash の通常updateは NotFound。
service/port は `ServiceError::{Conflict,NotFound,InvalidInput,Unavailable}` を返し、
driver の SQL error に個人の metadata を載せて外部へ流さない。

## 後続 Issue でそのまま使える API

worker/daemon の composition root で DB を所有し、呼出ごとに短い repository borrow を作る。
GUI/CLI/MCP は storage を直接呼ばず、既存の protocol/daemon 境界に従う。

```rust,ignore
use cm_core::papers::{NewPaper, PaperList, PaperService, PaperVisibility};
use cm_storage::{LibraryDb, LibraryDirectory};

// path は利用者が選択した専用絶対パス。テストでは temp path を渡す。
let directory = LibraryDirectory::prepare(path)?;
let mut db = LibraryDb::open(&directory)?;
let mut service = PaperService::new(db.papers());
let created = service.create(NewPaper::titled("手動登録"))?;
let mut edited = service.get(created.id)?;
edited.bibliography.journal = Some("Journal".into());
let updated = service.update(&edited)?;
let page = service.list(&PaperList { limit: 100, ..Default::default() })?;
let next = service.list(&PaperList {
    after: page.last().map(|paper| paper.id), limit: 100, ..Default::default()
})?;
service.soft_delete(created.id)?;
let trash = service.list(&PaperList { visibility: PaperVisibility::Deleted, ..Default::default() })?;
service.restore(created.id)?;
```

- #10: NewPaper/create/get/update/list/delete/restore を使用。limit は1..=500、既定50。
  cursor は最後の UUID（exclusive）、UUID昇順の keyset pagination。
  1回の get/list は一つの read transaction。複数ページは最新 committed 状態を読むので、
  途中の delete/restore/new insert を含めた固定snapshotではない。
- #12: `Bibliography.authors` は完全な順序付き contributor 列。metadata編集は取得済み Paper を変更する。
  collection/tag は基礎FK schemaを利用し、自Issueで操作port/serviceを追加する。
- #33: NewPaper/Bibliography/ExternalId/normalize_doi と Conflict を再利用できる。
  ImportService の preview/commit の認可・stale検証・複数文献一括transaction・マージは未実装。
  この単一文献transactionを一括importのatomic保証の代用にしない。
  #8 の update は revision/CAS を持たず、明示的な後の更新が有効になる。

本Issueの実行証拠と未実施範囲は [ISSUE_8](../validation/ISSUE_8.md) を参照。
