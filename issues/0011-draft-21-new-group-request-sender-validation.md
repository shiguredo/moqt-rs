# REQUEST_UPDATE の NEW_GROUP_REQUEST を送信側で検証する

- Created: 2026-09-10
- Completed: 2026-09-12
- Branch: feature/fix-new-group-request-sender-validation
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §9.20.20 (NEW GROUP REQUEST Parameter) の MUST NOT を送信側で守る。DYNAMIC_GROUPS 非対応の Track に対する `REQUEST_UPDATE` に `NEW_GROUP_REQUEST` を載せて送信できないようにする。

## 現状

`src/session/subscription/send.rs` の `send_update_for_subscription` は、`FORWARD` を `validate_forward`、`LOCATION_FILTER` を `location_filter_update`、`FILL_PARAMETERS` を `validate_outgoing_fill_parameters` で送信前に検証しているが、`NEW_GROUP_REQUEST` は検証していない。`subscription` が `DYNAMIC_GROUPS` を持たない場合でも送出できてしまう。

受信側の `handle_update_for_subscription` (`src/session/subscription/recv.rs`) は同じ MUST を実装している。送信側だけが素通りしている。

根拠 (draft-ietf-moq-transport-21 §9.20.20):

> "A subscriber MUST NOT send this parameter in REQUEST_UPDATE if the Track did not include the DYNAMIC_GROUPS Property with value 1. A subscriber MAY include this parameter in SUBSCRIBE without foreknowledge of support."

`tests/test_session/parameter_rules.rs` の該当テストは「送信は成功し server 側で拒否される」ことを前提にしており、送信側の MUST NOT 違反を固定している。

## 設計方針

`send_update_for_subscription` で `parameters.new_group_request().is_some() && !subscription.dynamic_groups` を検出し、`SESSION_PROTOCOL_VIOLATION` の `SessionError` を返す。SUBSCRIBE での `NEW_GROUP_REQUEST` は foreknowledge 無しでも許可されるため、REQUEST_UPDATE のみを対象とする。

検証は既存の送信前検証 (`validate_forward` / `location_filter_update` / `validate_outgoing_fill_parameters`) と同じく、`update_subscription_subscriber_delivery_timeouts_if_present` や `forward_state` などの楽観的状態更新より前に置く。同一 REQUEST_UPDATE に FORWARD 等を併載していても、拒否時は `Err` のみを返して他のパラメータをローカルに適用しない。

`tests/test_session/parameter_rules.rs` の既存 2 テスト (`request_update_with_new_group_request_without_dynamic_groups_rejected` / `request_update_with_new_group_request_dynamic_groups_zero_rejected`) は、受信側 `handle_update_for_subscription` の MUST
検証をカバーする唯一のテストである。送信側拒否のテストを追加したうえで、受信側検証は `ControlMessage::RequestUpdate` を直接構築して `recv_stream_message` に渡す形へ変更して維持する (0021 と同じ扱い)。

## 完了条件

- DYNAMIC_GROUPS 非対応 Track への `REQUEST_UPDATE` で `NEW_GROUP_REQUEST` が送信前に拒否されること
- 拒否時に同一 REQUEST_UPDATE の他パラメータ (FORWARD / LOCATION_FILTER / SUBSCRIBER_PRIORITY / delivery timeout) がローカルに適用されないこと
- 送信側拒否テストが追加され、受信側 MUST 検証テストが `ControlMessage::RequestUpdate` を直接構築する形で維持されていること
- SUBSCRIBE での `NEW_GROUP_REQUEST` は従来どおり許容されること

## 解決方法

送信側の REQUEST_UPDATE でも NEW_GROUP_REQUEST の MUST NOT を検証するようにした。

- `src/session/subscription/send.rs` の `send_update_for_subscription` に、`NEW_GROUP_REQUEST` があり
  `subscription.dynamic_groups` が false なら `SESSION_PROTOCOL_VIOLATION` を返す検証を、楽観的状態更新より前に追加した。
  拒否時は併載した FORWARD / LOCATION_FILTER / SUBSCRIBER_PRIORITY / delivery timeout をローカルに適用せず、
  送信イベントもクレジットも消費しない。SUBSCRIBE は foreknowledge 無しでも許可されるため対象外とした。
  あわせて `send_request_update` の doc に拒否条件を追記した。
- `src/session/types.rs` の `Subscription::dynamic_groups` の doc を実装に合わせて修正した
  (subscriber 側は受信した SUBSCRIBE_OK / PUBLISH の値、publisher 側は発行時の値を保持する。
  INCLUDE_PROPERTIES=0 の非対称も明記)。`send_subscribe` に残っていた
  「NEW_GROUP_REQUEST 検証は publisher 側で行う」という旧コメントも実態に合わせて修正した。
- `tests/test_session/parameter_rules.rs` に送信側拒否テスト
  `request_update_with_new_group_request_send_side_rejected_without_dynamic_groups`
  (拒否理由の検証、FORWARD / LOCATION_FILTER / SUBSCRIBER_PRIORITY / OBJECT_DELIVERY_TIMEOUT が
  適用されないこと、拒否後に正当な REQUEST_UPDATE を再送できること) と SUBSCRIBE 許容テスト
  `subscribe_with_new_group_request_without_foreknowledge_accepted` を追加した。
  既存の受信側 MUST 検証テスト 2 件は、送信側検証の追加により送信 API を使えないため
  `ControlMessage::RequestUpdate` を直接構築して `recv_stream_message` に渡す形に変更した。
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した。
