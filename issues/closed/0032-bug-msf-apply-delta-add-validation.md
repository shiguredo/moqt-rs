# MsfCatalog::apply_delta の add 経路でトラックを検証する

- Created: 2026-09-10
- Completed: 2026-09-10
- Branch: feature/fix-msf-apply-delta-add-validation
- Polished: 2026-09-10

## 目的

公開 API `MsfCatalog::apply_delta` が、add 操作で追加するトラックの MUST 制約を検証し、encode 時まで不正に気付けない非対称をなくす。

## 現状

`src/msf.rs` の `apply_delta` の add 経路は `track.clone()` を push するだけで、`validate_full_track` / `validate_media_track_fields` を呼ばない。`validate_after_delta` は一意性・initRef・グループ一致のみ検証する。

同じ検証は `validate_full_catalog_for_encode` / `validate_delta_for_encode` が `validate_full_track` を呼ぶことで行われ、`decode_track` は同等の検証をインラインで行う
(`MsfCloneTrack::into_track` も同等検証をインラインで行うが `lang` 検証を含まない)。そのため手組みの `MsfDeltaUpdate` で `is_live=true` かつ `track_duration=Some(..)`、
または role=video で codec なしのトラックを渡すと `apply_delta` は成功し、その後の `encode` で `InvalidCatalog` になる。

根拠: draft-ietf-moq-msf-01 §5.2 各フィールドの MUST。

## 設計方針

add 経路でも `validate_full_track(track)?` を呼ぶ。`apply_delta` の doc の Errors に検証内容を明記する。

## 完了条件

- add 経路で不正トラックが `apply_delta` 時点で拒否されること
- 既存の正常系が維持されること
- テストが `tests/test_msf/delta_apply.rs` に追加されていること

## 解決方法

`MsfCatalog::apply_delta` の add 経路でトラックの MUST 制約を検証するようにした。

- add するトラックに `validate_full_track` を適用し、encode 時まで不正に気付けない非対称を解消した (draft §5.2 各フィールド / §4.3.3 / §7.2 / §8.2 / §5.2.33)。
- add 操作内の全トラックを先に検証し、既存トラックおよびバッチ内の重複も全件検出してから追加することで、操作内の部分適用を避ける。
- `apply_delta` の doc の Errors に検証内容と、操作をまたぐ非原子性 (先行操作はロールバックされない) を明記した。
- `docs/IMPLEMENTATION.md` を更新した。
- `tests/test_msf/delta_apply.rs` に isLive + trackDuration / video の codec 欠如 / 不正トラック混在バッチ / 重複名バッチ / 複数トラック正常系 のテストを追加した。
- `CHANGES.md` の `[FIX]` にエントリを追加した。

残った制約 (スコープ外): clone の解決結果 (`MsfCloneTrack::into_track`) は `lang` 検証を含まないため、clone で不正な lang を設定すると encode 時に初めて失敗する。また `apply_delta` は操作をまたぐ原子性を持たない。
