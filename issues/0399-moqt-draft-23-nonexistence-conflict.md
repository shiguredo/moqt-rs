# 既知の不存在と Object の受信が矛盾した Track を検出する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-nonexistence-conflict
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §2.1.2、§3.2.2、§12.1 条件 10](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/object_properties.rs` の `ObjectPropertyTracker` は Prior Gap と既出 Object の矛盾を検出する。
一方、`src/stream/decoder.rs` の FETCH 範囲 entry は主に prior state を更新し、`crates/moq-sub/src/pipeline.rs` の `log_end_of_range` は範囲をログへ出す。
`src/session/data.rs` の `record_object_status_end` の説明は、不存在の後の Object は malformed ではないという draft 22 の前提を含む。

## 設計方針

`Session` が同一 peer・Full Track Name ごとの既出 Object 位置と不存在区間を所有し、別 FETCH / subscription / fill をまたいで照合する。単一 stream の Group 前進や 1 件の request の破棄だけでは、他の request が参照中の共有情報を消さない。
decoder は絶対 Location の entry を提供する責務を維持し、gap の意味判定を `Session` へ移す。`Session::recv_fetch_entry` と `DataPlaneHandle` の通知経路で decoded entry の内容を渡し、各 FETCH / fill の実効 Range Filters・要求範囲・応答で確定した終点、stream の FIN を関連付ける。応答より先に data や FIN が届いた場合は、終点が確定するまで境界に依存する判定を保留する。`log_end_of_range` の呼び出しだけで処理を終えない。
明示的な End of Non-Existent Range だけでなく、Range Filters の有無、応答境界、昇順・降順、FIN によって確定する暗黙 gap も区別する。
不存在後の Object と既出 Object を不存在とする逆順の矛盾を malformed Track とする。
Gone / Unknown / Timed-Out は不存在にしない。
矛盾時は既存の `Session::terminate_malformed_track` を使い、同じ Track の対応する要求を cancel する。共有状態は対応する subscription / FETCH の参照が残る間保持し、最後の request の forget と関連する受理済み data stream の終端が揃った時に破棄する。Session 終了時にも解放する。

## 完了条件

- 明示・暗黙 gap と Object がどちらの順に届いても矛盾を検出し、当該 Track の要求を cancel すること。
- Range Filter が作る gap と、Gone / Unknown / Timed-Out の後の正当な Object を誤拒否しないこと。
- 既存の Prior Gap 検証を維持し、別 request や過去 Group との照合情報を共有すること。1 件の request の破棄では他の参照を失わず、最後の request の forget と関連 stream 終端後、および Session 終了時に共有状態が解放されること。

## 依存・関連

- 先行: [0382](0382-moqt-draft-23-fetch-ok-request-ok.md)
- 先行: [0395](0395-moqt-draft-23-fetch-gone-range.md)
- 先行: [0396](0396-moqt-draft-23-fetch-range-delta.md)
- 先行: [0398](0398-moqt-draft-23-fetch-group-track-end.md)
- 関連: [0309](0309-draft-22-object-immutable-properties-search.md)
- 関連: [0352](0352-bug-subgroup-tracker-state.md)
