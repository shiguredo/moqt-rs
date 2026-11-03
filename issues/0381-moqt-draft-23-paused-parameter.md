# FORWARD を PAUSED に置き換えて状態の意味を反転する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.1.1、§3.3.3、§9.19.18](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message_parameter.rs` の `PARAM_FORWARD` は `0x10`、`forward()` は 0 を停止、1 を配送として扱う。
`src/session/types.rs` の `Subscription::forward_state` と `src/session/subscription/validation.rs` の配送判定も同じ意味で、既定値は 1 である。

## 設計方針

`PAUSED` (Type `0x11`、uint8) に置き換え、0 を unpaused、1 を paused、既定値を 0 とする。
codec、送受信 API、REQUEST_UPDATE / PUBLISH_STATE_NOTIFY、fill 開設判定、STOP_SENDING 後の再開判定、publisher / subscriber の構築箇所を一貫して変更する。
旧 `0x10` を新しい意味で受け入れない。

## 完了条件

- 省略・0・1・範囲外の encode / decode と Session の状態遷移を検証すること。
- pause 中は Object を配送せず、再開で STOP_SENDING 後の許可条件が正しく成立すること。
- fill とライブ配送が PAUSED の同じ意味を使い、旧 FORWARD の API・ワイヤ値が残らないこと。

## 解決方法

- `FORWARD` (Type 0x10、0 = 停止 / 1 = 配送、既定 1) を `PAUSED` (Type 0x11、uint8、
  0 = unpaused / 1 = paused、既定 0) に置き換えた (draft-ietf-moq-transport-23 §9.19.18)。
  `src/message_parameter.rs` の `PARAM_FORWARD` / `MessageParameters::forward` を
  `PARAM_PAUSED` / `MessageParameters::paused` に改名し、`value_encoding` /
  `validate_uint8_param_value` / `validate_param_encoding` を新しい型と値域 (0 / 1 のみ) に
  合わせた。範囲外の値は encode / decode とも `ProtocolViolation` で拒否する。
- 旧 Type 0x10 は `value_encoding` の未知型経路に落ちるため、新しい意味では受け入れない。
  encode 側も同じ経路で拒否され、送信側で旧 Type を組み立てることもできない。
- `src/message.rs` のメッセージ別許可集合 (`SUBSCRIBE_ALLOWED_PARAMS` /
  `SUBSCRIPTION_UPDATE_ALLOWED_PARAMS` / `REQUEST_UPDATE_ALLOWED_PARAMS` /
  `PUBLISH_ALLOWED_PARAMS` / `PUBLISH_STATE_NOTIFY_ALLOWED_PARAMS` /
  `SUBSCRIBE_TRACKS_ALLOWED_PARAMS`) を PAUSED に差し替えた。§9.19.18 の MAY appear 列挙
  (SUBSCRIBE / REQUEST_UPDATE / PUBLISH / PUBLISH_STATE_NOTIFY / SUBSCRIBE_TRACKS) は
  FORWARD の出現集合と同じであり、許可集合の形は変えていない。
- Session の状態を `Subscription::forward_state: u8` から `Subscription::paused: bool` に
  変えた。意味が反転して 0 / 1 の取り違えが起きやすいため、真偽値にして型で区別する。
  値域検証と変換は `src/session/subscription/validation.rs` の `validate_paused` が
  `Result<bool, SessionError>` を返す 1 か所に閉じ、既定値 (SUBSCRIBE / PUBLISH で省略) は
  `extract_paused_state` が unpaused (`false`) を返す。
- REQUEST_UPDATE / PUBLISH_STATE_NOTIFY で PAUSED を省略した場合は現在値を維持する
  (§9.19.18)。`send_update_for_subscription` / `send_publish_state_notify` /
  `send_ok_for_subscription` の pending 適用 / `handle_peer_publish_state_notify` /
  `handle_update_for_subscription` は、`MessageParameters::paused` が `Some` のときだけ
  状態を更新する。省略を既定値 0 で上書きしない。
- STOP_SENDING 後の再オープン許可を「購読を再開する REQUEST_UPDATE (paused が `true` から
  `false` へ変わる) が受理された場合」に置き換えた (draft-ietf-moq-transport-22 §11.3.2)。
  `send_ok_for_subscription` は `subscription.paused && !new_paused` のときだけ
  `stopped_outgoing_subgroups` を解除する。paused が変化しない更新 (unpaused からの
  paused 化や PAUSED 省略) では解除しない。
- fill の開設判定 `Session::maybe_open_fill_stream` は `paused: bool` を受け取り、paused なら
  開設しない。ライブ配送の `object_passes_filters` / `header_passes_filters` と同じ
  `Subscription::paused` を意味の正本とし、§3.1.1 の「paused な購読では Object を送らない」
  を両経路で共有する。§3.3.3 の Pass 評価のコメントも
  `Pass = (NOT Paused) AND Location Filters AND Range Filters` に更新した。
- テスト: codec は省略・0・1 の往復 (PBT) と、値域外・旧 0x10 の拒否 (encode / decode) を
  追加した。Session は既存テストを PAUSED の意味に反転して更新し、SUBSCRIBE / PUBLISH の
  状態設定、REQUEST_UPDATE / PUBLISH_STATE_NOTIFY の反映と省略時の維持、paused 中の
  Object 送信拒否、fill の開設抑止、STOP_SENDING 後の再開条件を検証している。
- `crates/moq-pub` / `crates/tokio-moq` のコメントと `skills/shiguredo-moqt/SKILL.md` の
  パラメータ表を PAUSED に合わせた。`docs/` の draft 22 記述の全面更新は
  draft 23 追従のドキュメント作業に委ねる。

## 完了条件の検証

- 省略・0・1 の encode / decode:
  `pbt/tests/prop_message_parameter.rs` の
  `paused_parameter_roundtrip_preserves_absence_and_values` が 3 状態を生成し、
  `MessageParameters::paused` が `None` / `Some(0)` / `Some(1)` を返すことと、ワイヤが
  count + 型 0x11 + uint8 値であることを固定する。
- 範囲外の encode / decode: `tests/test_message_parameter.rs` の
  `paused_out_of_range_rejected_on_encode` / `paused_out_of_range_rejected_on_decode`。
- 旧 FORWARD のワイヤ値を受け入れないこと: 同ファイルの
  `former_forward_type_is_unknown_on_decode` / `former_forward_type_is_unknown_on_encode`。
- Session の状態遷移: `tests/test_session/subscription/handshake.rs` の
  `subscribe_with_paused_sets_paused_state` / `publish_with_paused_sets_paused_state`、
  `tests/test_session/subscription/request_update.rs` の `request_update_full_cycle` /
  `multiple_request_updates_are_coalesced` /
  `send_request_update_rejects_invalid_paused_value` /
  `send_request_update_rejects_invalid_location_filter` (PAUSED 省略時に paused を
  維持すること) / `peer_request_update_with_invalid_paused_closes_session`、
  `tests/test_session/subscription/publish_state_notify.rs` の
  `notify_paused_round_trip_updates_paused_state` と省略時維持のテスト。
- pause 中は Object を配送しないこと: `tests/test_session/object_filter_pass.rs` の
  `paused_rejects_subgroup_object` / `paused_rejects_datagram` /
  `paused_rejects_end_of_group_datagram` / `paused_still_allows_publish_done`。
- 再開で STOP_SENDING 後の許可条件が成立すること:
  `tests/test_session/data_stream.rs` の `stop_sending_requires_resume_before_reopen` /
  `stop_sending_with_unpaused_subscription_blocks_reopen_without_resume` /
  `stop_sending_then_reset_still_blocks_reopen_until_resume` /
  `first_object_id_stop_sending_blocks_resolution_until_resume` /
  `shared_alias_resume_does_not_unlock_other_subscription`、
  `tests/test_session/subscription/request_update.rs` の
  `resume_allows_reopen_of_stopped_by_peer_subgroup` /
  `publish_originated_resume_allows_reopen_of_stopped_by_peer_subgroup`。
- fill とライブ配送が PAUSED の同じ意味を使うこと:
  `tests/test_session/fetch/fill.rs` の `subscribe_with_fill_and_paused_opens_nothing` /
  `subscribe_with_fill_opens_fill_stream` と、`Session::maybe_open_fill_stream` が
  `Subscription::paused` と同じ真偽値で判定する実装。
- 旧 FORWARD の API・ワイヤ値が残らないこと: ワークスペースに `PARAM_FORWARD` /
  `MessageParameters::forward` / `Subscription::forward_state` は残っていない。
  ワイヤ値 0x10 は上記の 2 テストで未知型として拒否されることを固定した。
