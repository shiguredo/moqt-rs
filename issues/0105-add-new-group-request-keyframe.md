# NEW_GROUP_REQUEST を映像キーフレーム要求に配線する

- Created: 2026-09-18
- Completed: {YYYY-MM-DD}
- Branch: feature/add-new-group-request-keyframe
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §3.5.1 / §9.20.20 の `NEW_GROUP_REQUEST` は、購読者が Group の途中で参加したときに新しい Group (= ランダムアクセス点) を要求する仕組みである。現在の publisher example は要求に `REQUEST_OK` を返すだけで、キーフレーム生成に結び付けていないため、購読者は次の周期キーフレームまで待たされる。

## 現状

- `examples/moqt-publisher/src/pipeline.rs` の `serve_peer_request` は `REQUEST_UPDATE` を受けて `REQUEST_OK` を返すだけ
- `examples/moqt-transport/src/moqt_client.rs` の `publish_track` は空の `TrackProperties` で PUBLISH するため、`DYNAMIC_GROUPS` (draft-ietf-moq-transport-21 §10.6) を告知できない
- `ClientEvent::RequestUpdate` の `IncomingRequestUpdate` は `parameters: MessageParameters` を公開しているため、`NEW_GROUP_REQUEST` の有無は example 側で判定できる
- session 層 (`src/session/subscription/recv.rs`) は `NEW_GROUP_REQUEST` と `DYNAMIC_GROUPS` の検証を実装済み
- 任意フレームでキーフレームを要求する経路は 0102 で追加する

## 設計方針

- 映像 Track の PUBLISH で `DYNAMIC_GROUPS=1` を告知する。`publish_track` に `TrackProperties` を渡せるようにする
- `REQUEST_UPDATE` の `NEW_GROUP_REQUEST` を受けたら、次の入力フレームで強制キーフレームを要求する
- 最小間隔 (例 300 ms) を設け、連続要求でキーフレームが乱発されないようにする (libwebrtc の `EncoderRtcpFeedback` と同じ)
- 0102 のキーフレーム強制経路とは関数呼び出し 1 つで接続できる形にする

## 完了条件

- `DYNAMIC_GROUPS=1` を告知した映像 Track への `NEW_GROUP_REQUEST` で、要求後に映像 Group が開始されること
- 最小間隔内の連続要求でキーフレームが乱発されないこと
- 既存の `REQUEST_UPDATE` (Forward / Range Filter) の応答挙動が変わらないこと
- 0102 のキーフレーム強制経路を使っていること
