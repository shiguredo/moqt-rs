# PBT で MSF の group 検証を固定する

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/test-msf-group-validation-pbt
- Polished: 2026-09-27

## 目的

MSF の renderGroup / altGroup のグループ内一致検証 (draft-ietf-moq-msf-01 §5.2.8 / §5.2.9) が PBT でカバーされていない。トラック生成がグループを他トラックと独立に生成するため、生成カタログがグループ検証の経路を実質通らず、回帰を検出できない。

## 現状

`pbt/tests/prop_msf.rs` の `sample_track` は `render_group` を `sample_optional_u32` で他トラックと独立に生成し、`alt_group` は生成しない。`full_catalog_roundtrip` などは `sample_track` を最大 5 件呼び、`(namespace, name)` の重複だけを排除する。

`noprop::sample_u32` は u32 全域を返すため、複数トラックが同じ `render_group` を持つ確率は実質ゼロである。そのためグループ内一致検証の成功経路も失敗経路も PBT では生成されず、グループ検証は単体テスト (`tests/test_msf/` 配下) のみに依存している。

また、グループの一貫性を生成器が保証していないため、生成カタログがグループ検証で拒否され得る状態のまま往復の性質テストを書いている。

## 設計方針

PBT のトラック生成にグループの一貫性を持たせ、グループ検証を性質として固定する。

- カタログ全体で renderGroup / altGroup の ID を先に決め、同じグループに属するトラックは同じ `targetLatency` / `buffers` を持つように生成する (グループ ID は少数の値域から選び、トラック間の衝突を意図的に起こす)
- グループの割当は `tracks` と `publishTracks` の両方に適用する (グループ検証は方向ごとに独立して実行されるため、片方だけでは成功経路・失敗経路のどちらかが未カバーになる)
- グループ内で片方だけが値を宣言する形 (省略との混在) も生成対象に含める
- 宣言値が異なるグループを意図的に生成し、`MsfCatalogDocument::decode` と `MsfCatalogDocument::encode` の両方が拒否する性質を別のテストで固定する
- 生成カタログが常にグループ検証を通ることを前提にできるようにし、往復の性質テストがグループ衝突で偶発的に失敗しないようにする
- delta 更新の add 操作は decode / encode 経路でグループ検証を実行しない (`src/msf.rs` の `decode_delta` / `validate_delta_for_encode`) ため対象外とする。delta 適用後の検証 (`MsfCatalog::validate_after_delta`) は単体テスト (`tests/test_msf/delta_apply.rs`) で固定済みのため PBT の対象外とする
- 生成器の変更範囲が重なる [issues/0147](../issues/0147-test-msf-codec-pbt.md) とは役割分担し、本 issue はグループ生成のみを対象とする (0147 は codec / role)

## 完了条件

- グループ内一致を満たすカタログ (tracks と publishTracks の両方を含む) の往復の性質テストが追加されていること
- 宣言値が異なるグループを `MsfCatalogDocument::decode` と `MsfCatalogDocument::encode` の両方が拒否する性質テストが追加されていること
- 省略との混在を受理する性質が固定されていること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
