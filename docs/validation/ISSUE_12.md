# #12 実行証跡

2026-11-06。現行基点 main `1c113292f88a4189f7f91c13cfdbe45eeb11d7ac`（#9 統合済み）。
Rust/Cargo 1.99.1、Python 3.14.0、macOS の T3 専用 worktree で実行。
実ユーザーの library / config / 他 worktree は使用していない。

## 実装と受け入れ条件

| 条件 | 実装 / 実行した検証 |
|---|---|
| Collection hierarchy CRUD/move/cycle | `CollectionService` + SQLite。自己/子/孫への move 拒否、rename/reorder、親変更、root削除時の子昇格 |
| 複数所属と安全な削除 | 重複追加/解除の冪等、所属 overlap。削除は自身の所属だけを解除し、完全なPaper/添付/残った所属を再open後にも保持 |
| Tag 正規化/衝突/CRUD | domain `TagKey` の Unicode空白・大文字・展開/上限、raw正規化/canonical serde拒否、create/rename衝突、ID/所属維持、tag削除 |
| #33 共通保存先 | 既存 `ImportDestination` を変更せず ID を検証、重複はset、最大100 tag、nil/missing/stale ID拒否、追加リンクのatomic rollback |
| 共通filter | Tag All/Any/empty/duplicate、collection直接所属、author/editor区別、year範囲、type、PDF metadata有無、全条件併用 |
| M1 keyword | 日本語短語、É/combining/Ω、書誌列・editor・外部ID・tag、ASCII大小文字、SQL wildcard/quote/backslash/injection文字列のliteral処理 |
| stable sort/cursor | ID/title/year/created_at/updated_at × 昇降2方向 × page size 1/2/3/5/11/12 の60通り。大量tie、NULL年、負/0年、境界UUID、JSON再読込、重複/欠落なし |
| cursor検証 | version/形状/UUID/nil/値型/別filter/別sort/未知field/1MiB超過拒否、page size変更可、境界行削除後の継続 |
| soft delete/restore | 通常queryと所属読取はtrashを除外、trash/all query、復元時に全書誌/順序付きcontributor/外部ID/所属/PDF metadataを保持 |
| persistence/rollback | private temp disk DBの再open、in-memory同schema。child移動後のcollection DELETE失敗、collectionと先行tag追加後の後続tag INSERT失敗をtriggerで起こしてrollbackを確認 |

新規テストは domain 1、core 1、storage integration 8。主なfixtureは
`crates/storage/tests/library_organization.rs`。既存の attachment は #8 schema の **metadata fixture**、追加した MIME 回帰は #9 の
実 managed-ingestion API を使う。shared blob の実在と integrity report も確認する。
いずれも合成 bytes であり valid PDF の parsing/rendering を実証する fixture ではない。

## 完了したコマンド

すべて exit 0。workspace は60 unit/integration tests + 1 compile-fail doctest。

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --workspace --locked
python3 scripts/check_architecture.py
python3 scripts/check_migrations.py --base origin/main
python3 -m unittest discover -s scripts -p 'test_*.py' -v
git diff --check
```

architecture は12 cratesの許可依存方向/platform isolation、migrationは既存2件のID/checksumと
base不変を確認。Python17テストはarchitecture/migration validatorとPDFium toolのfixture/pin検証。
新規migrationは不要。Cargo.lock差分は `cm-core` の既存固定serdeへの依存edge1行だけで、
依存バージョン/source/checksumの変更はない。

## API 確認と未実施範囲

`find-docs` skillの Context7 CLIでrusqliteをresolveし、`/rusqlite/rusqlite/v0.41.1` と
`/websites/rs_rusqlite_rusqlite` のdocsを照合。version-specific結果は主にchangelogだったため、
[params_from_iter](https://docs.rs/rusqlite/latest/rusqlite/fn.params_from_iter.html) と
[Transaction](https://docs.rs/rusqlite/latest/rusqlite/struct.Transaction.html) の公式APIに加え、
ローカル固定0.41.1の `src/params.rs` / `src/transaction.rs` で実際のsignatureとrollback-on-dropを確認した。

契約と制限は [ADR](../ADR/library-organization-query.md) を参照。
M1 keywordはscanによる書誌substring。FTS/PDF本文/notes/relevance/性能目標は未実装/未測定。
#33のpreview/merge/作成を含むatomic import、GUI、daemon認可は対象外。
#9 file store の実装は基点に含まれ、ここでは既存 ingestion API と query の連携を検証した。
optional native GPUI/PDFium runtime / signing laneはこの変更でローカル再実行していない。
独立した異モデルreviewとmergeはrootがこのPRのexact headに対して実施する。

## Integration after #9

Integrated merged #9 (`1c11329`) before final review. Both feature appendices and module
registrations are preserved; no applied SQL/checksums or dependency versions changed.
Root passed integrated workspace fmt/clippy/test/build, architecture, migration preservation
and Python validators. Workspace tests including compile-fail doctest: 61.

## Independent review response

P3 MIME: query が ASCII case / SP+HTAB / parameters を扱う base-type 比較を所有し、
実 #9 API から canonical / case / parameter variants と非PDF metadata を保存して照合する。
P3 serde: raw 入力の正規化と永続キーの読込を区別し、非正規キーを TryFrom/serde で拒否する。
P3 docs: 現行 base / 統合テスト / 件数へ更新した。
P3 oversized title: 1 MiB cursor 制限に伴う既知制約を保持し、明示的 InvalidInput と
ID 順で取得を継続できることを回帰テストで検証する。入力の title 上限は #33/#30 の後続契約。
