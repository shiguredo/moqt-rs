# 既知の不存在と Object の受信が矛盾した Track を検出する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-nonexistence-conflict
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §2.1.2、§3.2.2、§12.1 条件 10](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/object_properties.rs` の `ObjectPropertyTracker` は Prior Gap と既出 Object の矛盾を検出する。
一方、`src/stream/decoder.rs` の FETCH 範囲 entry は主に prior state を更新し、`crates/moq-sub/src/pipeline.rs` の `log_end_of_range` は範囲をログへ出す。
`src/session/data.rs` の `record_object_status_end` の説明は、不存在の後の Object は malformed ではないという draft 22 の前提を含む。

## 設計方針

FETCH で確定した不存在の範囲と、同じ Full Track Name に対する別 FETCH / subscription の Object を照合するための通知・状態連携を用意する。
明示的な End of Non-Existent Range だけでなく、Range Filters の有無、応答境界、昇順・降順、FIN によって確定する暗黙 gap も区別する。
不存在後の Object と既出 Object を不存在とする逆順の矛盾を malformed Track とする。
Gone / Unknown / Timed-Out は不存在にしない。

## 完了条件

- 明示・暗黙 gap と Object がどちらの順に届いても矛盾を検出し、当該 Track の要求を cancel すること。
- Range Filter が作る gap と、Gone / Unknown / Timed-Out の後の正当な Object を誤拒否しないこと。
- 既存の Prior Gap 検証を維持し、状態の破棄・保持範囲を明記すること。

## 依存・関連

- 先行: [0382](0382-moqt-draft-23-fetch-ok-request-ok.md)
- 先行: [0395](0395-moqt-draft-23-fetch-gone-range.md)
- 先行: [0396](0396-moqt-draft-23-fetch-range-delta.md)
- 先行: [0398](0398-moqt-draft-23-fetch-group-track-end.md)
- 関連: [0309](0309-draft-22-object-immutable-properties-search.md)
- 関連: [0352](0352-bug-subgroup-tracker-state.md)
