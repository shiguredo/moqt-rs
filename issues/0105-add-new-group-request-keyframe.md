# NEW_GROUP_REQUEST を映像キーフレーム要求に配線する

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/add-new-group-request-keyframe
- Polished: 2026-09-28

## 目的

draft-ietf-moq-transport-21 §3.5.1 / §9.20.20 の `NEW_GROUP_REQUEST` は、購読者が Group の途中で参加したときに新しい Group (= ランダムアクセス点) を要求する仕組みである。現在の publisher example は要求に `REQUEST_OK` を返すだけで、キーフレーム生成に結び付けていないため、購読者は次の周期キーフレームまで待たされる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` の `ClientEvent::RequestUpdate` 分岐は、受信した `REQUEST_UPDATE` に `REQUEST_OK` を返すだけである (`serve_peer_request` は SUBSCRIBE / FETCH を扱う関数であり、`REQUEST_UPDATE` はここでは扱わない)
- `examples/tokio-moq/src/moqt_client.rs` の `publish_track` は空の `TrackProperties::new()` で PUBLISH するため、`DYNAMIC_GROUPS` (draft-ietf-moq-transport-21 §10.6) を告知できない
- `ClientEvent::RequestUpdate` の `IncomingRequestUpdate` は `parameters: MessageParameters` を公開しており、`MessageParameters::new_group_request()` (draft-ietf-moq-transport-21 §9.20.20 のパラメータ型 0x32) で `NEW_GROUP_REQUEST` の有無と値を example 側で判定できる
- session 層は `NEW_GROUP_REQUEST` と `DYNAMIC_GROUPS` の検証を実装済みである。受信側は `src/session/subscription/recv.rs` の `handle_update_for_subscription` が `DYNAMIC_GROUPS=1` でない Track への `NEW_GROUP_REQUEST` を、送信側は `src/session/subscription/send.rs` の `send_request_update` が同条件を拒否する。`DYNAMIC_GROUPS` の値域 (0 / 1 のみ) は `src/track_properties.rs` の `validate_track_property_value_range` が encode / decode の両方で検証する
- 任意フレームでキーフレームを要求する経路は 0102 で追加する。0102 は未完了であり、本 issue の実装は 0102 の完了を前提とする

## 設計方針

- 映像 Track の PUBLISH で `DYNAMIC_GROUPS=1` を告知する。`publish_track` に `TrackProperties` を渡せるようにし、映像 Track だけ `TrackProperty { prop_type: PROP_DYNAMIC_GROUPS, value: TrackPropertyValue::VarInt(1) }` を載せる (音声 / カタログ Track は空のまま)。告知は Original Publisher としての PUBLISH で行い、SUBSCRIBE_OK の Track Properties は変更しない
- `NEW_GROUP_REQUEST` の値が 0 または現在の映像 Group ID より大きい場合にだけ、次の入力フレームで強制キーフレームを要求する (draft-ietf-moq-transport-21 §9.20.20)。値が現在の Group 以下なら何もせず、`REQUEST_OK` は従来どおり返す。次の Group ID はパラメータ値と一致させる必要はない (同 §9.20.20)
- 最小間隔 (例 300 ms) を設け、連続要求でキーフレームが乱発されないようにする。最小間隔内の要求は破棄せず保留し、間隔経過後の最初の入力フレームで強制キーフレームを要求する (draft-ietf-moq-transport-21 §9.20.20 が "The Original Publisher MAY delay the NEW_GROUP_REQUEST subject to implementation specific concerns, for example, achieving a minimum duration for each Group." と定める遅延)
- `ClientEvent::RequestUpdate` で `new_group_request()` が `Some` になるのは session 層の検証を通過した要求だけであり、`DYNAMIC_GROUPS=1` を告知した映像 Track に限られるため、対象 Track の判定は値の確認だけで足りる
- 強制キーフレーム要求は 0102 で追加される `encode()` のキーフレーム強制引数への呼び出し 1 つで実現できる形にする

## 完了条件

- `DYNAMIC_GROUPS=1` を告知した映像 Track への `NEW_GROUP_REQUEST` で、要求後に映像 Group が開始されること
- 最小間隔内の連続要求でキーフレームが乱発されないこと
- 既存の `REQUEST_UPDATE` (Forward / Range Filter) の応答挙動が変わらないこと
- 0102 のキーフレーム強制経路を使っていること
