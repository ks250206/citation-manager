# #12: Collection / Tag / 共通 library query

状態: 採用。#8 の schema と Paper API に互換的に追加する。migration、既存 checksum、
依存バージョン、ユーザー設定は変更しない。GUI / import / file store / daemon 認可は実装しない。

## 境界と API

domain の `Collection` / `Tag` / `TagKey` は driver に依存しない。
core の `collections::{CollectionLibrary, CollectionService}`、`tags::{TagLibrary, TagService}`、
`library_query::{LibraryQueryRepository, LibraryQueryService}` が worker 側の同期 port / service。
storage が `LibraryDb::{collections,tags,library_query}` の短い borrow で SQLite adapter を返す。
通常の `PaperService` / `PaperList` はそのまま利用できる。新 query は完全な `Paper` を返し、
全書誌情報、順序付き contributor（editor を含む）、外部 ID、lifecycle timestamp を保持する。
UI / transport はこの API のために直接 SQLite を開いてはいけない。

## Collection と削除

- storage が UUIDv7 を生成する。親なしは root。名前は下記の表示名規則、sort_order は i64。
  同名の sibling は許可する。`list` は flat な `(parent_id, sort_order, id)` 昇順、root が先。
  UI の tree traversal は parent ID から構築する。Tag/Collection の管理一覧は全件を返す。
- `update(id, CollectionInput)` が rename / reorder / move。親の存在と自己/全子孫への循環を
  IMMEDIATE transaction 内で検査する。再帰 CTE は UNION を使い、既存 cycle にも無限反復しない。
  外部の任意 SQL writer による階層破壊を防ぐ API ではない。daemon writer 制御は #25。
- **削除時は、直接の子を削除対象の親へ移動する（root を消した場合は子が root になる）。**
  この移動と collection 削除は単一 transaction。子孫 collection は削除しない。
  schema の ON DELETE CASCADE が消すのは削除 collection 自身の `paper_collections` のみ。
  文献、他 collection の所属、tag、添付、本文、注釈は一切削除しない。
- 文献は任意個の collection / tag に所属できる。追加と解除は存在する active paper と
  collection/tag を要求し、重複追加と既にない所属の解除は冪等。存在しない対象は NotFound、nil ID は InvalidInput。
  collection/tag 自体の再削除は NotFound。
- paper の削除/復元は既存 `PaperService::soft_delete/restore`。関連を保持し、復元すると
  所属も再表示する。trash の所属読取/編集は NotFound。collection/tag 自体の削除は trash にある
  文献の該当所属も解除するが、文献を消さない。Tag 削除も tag とその所属だけの cascade。

## 共通 TagKey V1（#33 と共有）

正規化は `cm_domain::TagKey::new` の一箇所に集約する。ImportService / 各入力 adapter は
別のタグ正規化を実装しない。DB の `normalized_name` UNIQUE が衝突を Conflict として返し、
create/rename による自動 merge/upsert はしない。検索は `TagLibrary::find(&TagKey)`、
raw 名の便宜 API は `TagService::find`。名前/色の変更は ID/所属を保持する。

1. 入力は最大4096 UTF-8 bytes。非 whitespace の制御文字（NUL等）を拒否する。
2. Rust `split_whitespace` の Unicode White_Space を前後で取り除き、連続空白を ASCII space 一つにする。
   改行、tab、全角空白、NBSP も同じ扱い。表示名はこの結果を保ち、大小文字は残す。
3. キーは各文字の Rust `char::to_lowercase` を連結する。Unicode の展開を許す（`İ` → `i` + combining dot）。
   表示名とキーはいずれも1..=256 Unicode scalar。展開後に上限を超える入力も InvalidInput。
4. NFC/NFKC、幅/アクセント統一、locale、full case folding はしない。
   `é` と `e` + combining acute、`Ａ` と `A`、`ß` と `ss`、`ς` と `σ` は別キー。
   日本語や zero-width format character はこの規則で変わらない部分を保持する。

raw 入力は `TagKey::new` で正規化する。`TryFrom<String>` / serde 読込は
すでに正規化されたキーだけを受け付け、`"FOO "` のような値は拒否する。256 scalar 上限は UTF-8 byte 数とは異なる。
Unicode tables は固定 Rust toolchain に由来する。将来の更新でキーが変わる場合は、既存キーを黙って
再計算せず、衝突を検出する追記 migration と契約更新が必要。空/空白のみは InvalidInput。
Collection 表示名も1–2の同じ規則と256 scalar上限を使い、小文字化しない。
Tag color は None または ASCII `#RRGGBB`、保存する大文字/小文字は入力どおり。

## ImportDestination の統合契約

`core/import.rs` の既存 `ImportDestination { collection: Option<CollectionId>, tags: Vec<TagId> }` は
変更しない。`collections::DestinationLibrary` は存在確認と、既存 active paper への atomic な
**追加**所属を提供する。None/空は追加なし、重複 tag ID は set、最大100 ID（重複を含む）。
`validate_destination` は preview 用に使えるが、commit 時にも存在確認する。stale ID は NotFound。
検証と全リンク追加は同じ write transaction で行い、途中の失敗は一部所属も残さない。

これは ImportService 自体でも、paper 作成と所属追加の一括 transaction でもない。
#33 は public の create と apply_destination を順に呼ぶだけで atomic import を主張してはいけない。
storage 内の `repositories::collections::{validate_destination,apply_destination}` は
`pub(crate)` の transaction 非所有 helper なので、#33 の SQLite commit adapter の単一 transaction 内に
再利用できる。helper の呼出側が transaction / rollback を所有する。
preview の認可、stale paper revision、重複判定、merge、batch commit は #33 / #25 が所有する。

## M1 LibraryQuery と #18 adapter 契約

`LibraryQuery { filter, sort, after, limit }` に全入口の検索/一覧条件を集約する。
同一 filter 内の条件は AND。Tag は All / Any の切替で、空リストはいずれも条件なし、重複は set。
存在しない filter ID は該当なし。collection は **直接所属** のみで子孫を暗黙に含めない。
年は inclusive from/to（逆転は InvalidInput）、未指定年は年範囲の対象外。
type は保存値の完全一致、author は role=`author` の given/family、両順の結合名、literal の部分一致。
editor は author filter に含めないが keyword の対象になる。
has_pdf は `kind='pdf'` と base MIME type `application/pdf` の添付 metadata の EXISTS / NOT EXISTS。
base type はセミコロンより前を取り、前後の ASCII space/tab を除去して ASCII 大小文字を区別せず照合する。
従って `application/PDF` や `Application/Pdf; version=1.7` も対象。#9 は入力 MIME を保持し、query がこの比較規則を所有する。
ファイルの実在/可読/valid PDF は保証しない。#9 の実 managed-ingestion API から添付した表記違いも integration test で照合する。

M1 keyword は Unicode whitespace 区切りの各 token の **literal substring** を AND、
各 token は次のフィールドのいずれかにあれば一致する。

- title / abstract / journal / DOI / URL / citation_key / type / publication_date / language /
  volume / issue / pages / year の文字列表現
- 全 contributor の given/family/literal と両順の結合名
- 外部 ID の namespace / value、所属 tag の表示名 / 正規化キー

SQLite builtin `lower` を使うため ASCII case-insensitive、その他の Unicode は literal。
これは TagKey の Unicode 小文字化とは用途が異なり、書誌情報の原文を正規化して保存しない。
`%` / `_` / backslash / quote は演算子にならず文字として照合する（`instr` と bound parameters）。
検索語も ID も全て bind し、SQL の列名/方向は閉じた enum だけから選ぶ。
DSL、FTS、ランキング、stemming、PDF本文、注釈/notes、extra_json/ORCID の検索は未実装。
日本語1–2文字も substring として検索可能だが、index/速度の目標を満たしたとは主張しない。
M1 は scan を許す。limit は返却件数を制限するもので scan時間の上限ではない。

#18 はこの core port を実装/合成し、同じ filter/visibility/完全な Paper と stable sort の契約を保つ。
全文の syntax、score、snippet、attachment/page hit は別の拡張契約で扱う。
M1 の文字列を MATCH にそのまま渡して意味を変えたり、metadata-presence を PDF hit と見なしてはいけない。
検索専用 index / trigger は #18 の追加 migration で所有する。

## ページングと上限

- limit は1..=500（default50）。filter tags は最大100 ID、nil ID 不可。
  keyword / author / type は各最大1024 UTF-8 bytes、Some(blank) とNULを拒否。keyword 最大16 token。
  呼出側は条件なしを None で渡す。author/type は有効な入力文字列をそのまま照合する。
- sort は ID / title / year / created_at / updated_at、昇降両順。
  文字列は BINARY、year の NULL は両方向とも最後。最後の tie-breaker は同方向の canonical UUID。
  title の locale-aware collation や relevance order はない。
- `LibraryPage.next` は追加行がある場合だけ返す。最後の row の sort key / ID と厳密な filter / sort を
  versioned opaque JSON cursor に保持する。復元は `LibraryCursor::from_encoded`、取得は `as_str`。
  version / 構造 / UUID / key型 / filter / sort を検証し、別 query の流用は InvalidInput。
  page size だけは変更可能。tag順などの filter 表現を途中で変える場合も最初から検索する。
- cursor は最大1 MiB（生成時も検証）。極端に大きい title（JSON escape による拡大も含む）が
  境界になる title sort はこの上限で InvalidInput を返す既知制約がある。ID 順等の sort は利用できる。
  既存タイトルのデータ上限は #33/#30 の入力契約で別途決める。この PR は cursor の上限を維持し、
  oversized title での明示的な失敗と ID 順への切替を回帰テストで固定する。
  cursor は認可 token / 署名ではなく、内部 metadata を含むためログに記録しない。
  caller が valid な構造の位置を生成してもアクセス権は増えない。認可と library の選択は #25。
- 一ページの ID 選択と完全 Paper の読取は同じ read transaction。
  複数ページ間は latest committed state。削除された anchor は格納 key により継続可能だが、
  metadata の並べ替え/復元/追加に対する固定 snapshot や一度だけの列挙は保証しない。

検証と API evidence は [ISSUE_12](../validation/ISSUE_12.md)。
