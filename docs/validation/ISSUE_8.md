# #8 SQLite / CRUD 検証

2026-11-06、implementation agent の isolated worktree、base `d35b22996a74b153d959ce0aa370ee4a9bffaac2`。
macOS 27.2 arm64、Rust 1.99.1、Python 3.14.0。対象の実装/APIは
[sqlite-library ADR](../ADR/sqlite-library.md) を参照。

## 実行結果

| command | 結果 |
|---|---|
| `cargo fmt --all -- --check` | passed |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | passed |
| `cargo test --workspace --locked` | passed。元 #7 contract/compile-fail test を維持 |
| `cargo build --workspace --locked` | passed、12 library crate |
| `python3 scripts/check_architecture.py` | passed、依存方向・driver所有・純契約層を維持 |
| `python3 scripts/check_migrations.py --base origin/main` | passed、2件のimmutable ID/SHA-256・base registry保持 |
| `python3 -m unittest discover -s scripts -p 'test_*.py' -v` | passed、13件 |
| `git diff --check` | passed |

`crates/storage/tests/library.rs` は実ファイルDBを temp path に作り、PaperService と実repositoryで
全書誌項目、Unicode、JSON の未知項目と u64、順序付き複数roleの著者、原文external IDsを保存し、
connection破棄→再open後の完全一致を確認する。
CRUD、型付きmissing/validation/conflict、冪等delete/restore、trash visibility、keyset pagination、
別connectionのWAL/FK、metadataのみ変更時のperson ID保持と添付保持、注釈source=ai/
未解決reason/位置未捏造、page labelとindex、SQLのFK/state制約を確認する。
DOI表記違い・引用キー・external-ID置換途中の失敗で、既存metadata/著者/IDが戻ることを確認する。

migration unit tests は製品のregistry/runnerを呼び、初期bootstrap、001の旧DB→002へのupgrade、
実際のSQLite online backupによるmemory snapshot hook、checksum不一致、未知future version、
欠落version、未version管理DB拒否、hookエラー、hook中の履歴変更、pending batch途中のSQL失敗を検証する。
失敗時には先行pending DDL/DML/historyまでrollbackし、実ファイルのclose/reopen後もschema/dataを保持する。
factory接続（file/new/reopen/in-memory）でorphan FK writeが実際に拒否されることも確認する。

`path_safety.rs` は新規directory=0700・DB/WAL/SHM=0600/own UID/single linkを確認し、
相対/親参照/dot、directory/ancestor symlink、DB/各sidecar symlink、hardlink、緩い権限、
writable ancestor、非通常file、open後に変更したdirectory権限を拒否する。
拒否時に外部targetや既存permissionを変更しないことを確認する。全てtemp pathのみ。

初回作業中のcompileではAttachmentのu64 FromSql未対応、test helperの名前shadow、
clippyではtestの不要なdropが見つかった。型変換/helper名/borrow scopeを修正して全検証を再実行した。

## 依存と公式API確認

共有済み rusqlite bundled/backup をそのまま使用する。storage に追加した依存:
`uuid` の v7 feature（internal ID生成）、`serde_json`（JSON保存）、`sha2 = 0.10.9`（migration checksum）、
`rustix = 1.1.6` process feature（unsafeなしのeffective UID取得）、dev `tempfile = 3.27.0`（隔離実DB）。
全て既存 lock に存在する版に固定した。domain/core の serde_json はJSON value/正規化で実使用する通常依存へ移した。
`cargo generate-lockfile` を実行し、意図しない cc 1.11.1への更新は
`cargo update -p cc --precise 1.11.0` で元の版に戻した。既存package versionの変更なし。
tempfile の許容範囲 `>=0.3,<0.5` 内で getrandom edge は既存0.3.4を再利用する解決になった。
lockを手で合成せず、最終状態を --locked で検証した。

find-docs skill: Context7 library解決とrusqlite v0.41.1 queryを実行。
queryは主に古いchangelogを返したため、公式 versioned docsをcurlで取得してAPI記述を確認した:

- [rusqlite 0.41.1 Connection](https://docs.rs/rusqlite/0.41.1/rusqlite/struct.Connection.html):
  open_with_flags、transaction_with_behavior、unchecked_transaction、pragma_update_and_check。
- [uuid 1.29.0 Uuid](https://docs.rs/uuid/1.29.0/uuid/struct.Uuid.html): now_v7のstd/v7 featureとprocess内生成順。
- [rustix 1.1.6 geteuid](https://docs.rs/rustix/1.1.6/rustix/process/fn.geteuid.html)、
  [sha2 0.10.9 Digest](https://docs.rs/sha2/0.10.9/sha2/)、
  [tempfile 3.27.0 tempdir](https://docs.rs/tempfile/3.27.0/tempfile/fn.tempdir.html)。
- [SQLite open flags](https://www.sqlite.org/c3ref/open.html)、[WAL](https://www.sqlite.org/wal.html)。

## 検証の限界と引渡し

DB I/O/SQLite制約/再open/Unix権限はこのmacOSで実行した証拠。
GUI/daemon/CLI/ネットワーク経由の実行ではなく、UIで保存→再起動→表示した証拠ではない。
GPUI/PDFのコード/worker/interfaceは変更していない。native dependency feature laneはCIに委ね、
GUI window、PDFium runtime/ABI、原本PDF/file store、製品バックアップ/復旧、認可、
single-writer daemon、import preview/merge/一括transaction、FTS検索はこのIssueの完了範囲ではない。
製品backupは #9、UIは #10、PDFは並行 #11、daemon lockは #25、共通importは #33。
異モデル非対話reviewはrootがこのPRのexact headに対して実施する。自己reviewを代用にしない。
CI状態はPR作成後に一度確認して引き渡す。ローカルpassedをCI passedとは記さない。
