# 既知の不存在と Object の受信が矛盾した Track を検出する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
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
- 関連: [0309](0309-draft-23-object-immutable-properties-search.md)
- 関連: [0352](0352-bug-subgroup-tracker-state.md)

## 解決方法

`Session` が Full Track Name ごとの `PeerTrackHistory` を所有し、既出 Object 位置、不存在区間 (Group 全域 / Group 内 Object ID 区間)、Prior Group / Object ID Gap の検証状態を subscription / FETCH / fill fetch stream で共有するようにした。

- `src/session/track_history.rs` を追加し、Object の存在記録と不存在範囲の記録を相互に照合する。不存在区間内の Object 受信と、既出 Object を覆う不存在通知の両方を `MalformedTrack` とする (§2.1.2、§12.1 条件 10)。
- `Session::recv_fetch_entry` が `DecodedFetchEntry` を受け取り、明示的な End of Non-Existent Range と暗黙 gap を解釈する。
- 暗黙 gap は Range Filters の有無 (§3.2.2)、要求の Start / End、Group Order、FIN から求め、要求範囲へ切り詰める。降順で Start と End の Group が異なる場合は §3.2.2 の先頭・末尾の期待 Location を基準にする。
- 応答 (FETCH_OK の LARGEST_OBJECT) より先にデータや FIN が届いた場合、`PendingFetchTail` に先頭 / 末尾 gap と切り詰め待ちの範囲を保持し、FETCH_OK の受信時に確定する。
- fill fetch stream は FETCH_OK を持たないため、Largest Object が判明している場合だけ暗黙 gap を判定する (`FillContext`)。
- End of Gone / Unknown / Timed-Out Range は存在を否定しないため記録しない (§2.1.2)。
- 矛盾検出時は既存の `Session::terminate_malformed_track` で同じ Track の subscription / FETCH を cancel する (§12.1)。
- 履歴の寿命は request と受理済み受信 data stream の参照カウントで管理する。1 件の request の `forget_*` では他の参照が残る間保持し、最後の参照の解放で破棄する。Session の破棄時にも解放される。
- `FetchStreamDecoder` から `ObjectPropertyTracker` を除去し、Prior Gap と不存在区間の意味判定を `Session` に移した。decoder は絶対 Location の解決と書式・Property スコープの検証を維持する。
- 公開 API の変更: `Session::recv_fetch_entry` と `DataPlaneHandle::recv_fetch_entry` が `&DecodedFetchEntry` を受け取る。`Fetch` に `has_range_filters` を追加した。`crates/moq-sub` と `crates/tokio-moq` の呼び出しを追随させた。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| 明示・暗黙 gap と Object の双方向の矛盾検出と、当該 Track の要求の cancel | `tests/test_session/fetch/nonexistence.rs` の gap 系 4 テスト |
| Range Filter が作る gap と Gone / Unknown / Timed-Out の後の Object を誤拒否しない | 同ファイルの 3 テスト |
| 既存の Prior Gap 検証と別 request・過去 Group の照合情報の共有 | 同ファイルの 2 テスト |
| 他の参照が残る間の保持と、最後の request の forget 後の解放 | 同ファイルの 1 テスト |
| 昇順・降順・FIN・応答前データの保留による暗黙 gap の区別 | 同ファイルの 4 テスト |
| fill fetch stream をまたいだ履歴の共有 | 同ファイルの 2 テスト |
| Group をまたぐ gap・要求範囲への切り詰め・降順の End of Range の範囲計算 | `src/session/track_history.rs` の単体テスト |

- 明示 gap と Object: `explicit_missing_range_then_object_cancels_track_requests`、`object_then_explicit_missing_range_cancels_track_requests`。
- 暗黙 gap と Object: `implicit_gap_between_objects_is_recorded_as_missing`、`implicit_gap_covering_received_object_is_rejected`。
- 誤拒否しないこと: `range_filter_gap_does_not_reject_later_object`、`fill_range_filter_gap_does_not_reject_later_object`、`end_of_range_other_than_non_existent_does_not_reject_object`。
- 共有と寿命: `prior_gap_is_shared_across_requests`、`past_group_object_is_shared_with_new_fetch`、`track_history_survives_one_forget_and_is_released_after_last`。
- 境界と FIN: `implicit_leading_and_trailing_gaps_are_recorded`、`implicit_trailing_gap_is_recorded_at_fin`、`trailing_gap_is_deferred_until_fetch_ok_arrives`、`descending_multi_group_gap_is_clamped_to_request`。
- fill: `fill_stream_shares_track_history`、`fill_range_filter_gap_does_not_reject_later_object`。
