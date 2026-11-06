# 実装と統合

Issue ごとの isolated worktree / `feat/issue-N` を使い、対象範囲だけを PR にする。
現段階の境界は [ADR](ADR/boundaries.md)、実行分担は [TEST_MATRIX](validation/TEST_MATRIX.md) を参照。

## 共有契約と所有

各機能 PR が自分の module、`lib.rs` の登録、manifest の dependency/feature、fixtures、migration の追記を所有する。
protocol と command は機能別 module を追加する。protocol の全操作 enum や巨大な全 command registry を
先に凍結しない。IPC envelope/JobStatus は #25、command ID/keymap は #16。
既存契約の意味/認可/互換性を変える場合は先行する契約 PR で合意し、その統合後に依存実装を進める。
独立した additive module/manifest 追記の並行開発は許可する。全 branch の開発停止は要求しない。

## migration

ファイル名: `YYYYMMDDTHHMMSSZ_issue-N_SSS_description.sql`。
例: `20261106T080000Z_issue-8_001_initial.sql`。timestamp は UTC の有効日時、Issue は正整数、
sequence は 001 からの3桁、description は小文字英数字と `-`。
identity は timestamp + issue + sequence で immutable。同じ identity で別 description も衝突として拒否する。

新規 SQL とその SHA-256 を `crates/storage/migrations/manifest.json` に一緒に追加する。
適用済み migration は変更/削除/改名しない。共有された既存 migration も追記 migration で修正する。
#8 の runner は applied migration ID/checksum を DB に保持し、適用時にも照合し、不一致なら停止する。
#7 の validator は SQL を実行しない。空 registry は実 schema を実装したという意味ではない。

```sh
python3 scripts/check_migrations.py --base origin/main
python3 -m unittest discover -s scripts -p 'test_*.py' -v
```

validator は名前、calendar、ID 衝突、未登録/欠損、SHA-256、symlink を検査する。
`--base` は既存 registry の checksum の変更/削除を拒否する。比較なしでは checksum を同時に
書き換えた改変を検出できないため、PR/統合時は必ず base 指定を使う。

## 1 PR ずつの統合

1. merge 担当は最新 main と対象 PR の exact head を確認する。他の feature worktree は操作しない。
2. 対象 PR を現行 base に更新し、独立追加を残して module/manifest/migration の衝突を解消する。
3. `cargo generate-lockfile` で現行 base に基づき lock を再生成し、依存差分を確認する。
   lock を手で継ぎ合わせない。意図しない更新があれば pin/変更理由を確認して解消する。
4. README の fmt/clippy/test/build、architecture/migration validators と該当 platform lane を再実行する。
   意味が変わった head は異モデル read-only review を再実施し、CI/実機の未実施を passed にしない。
5. 保護ルール/承認を守り、検証済み head の PR を一つだけ統合する。続く PR は新しい main で同じ手順。

各 PR の独立した異モデル review は必須。#7 の実装 agent は PR 作成まで担当し、root が異モデル review を手配する。
未実施の UI/PDFium/署名をチェック済みの acceptance box にしない。秘密/credential/文献本文をログや Issue に貼らない。
