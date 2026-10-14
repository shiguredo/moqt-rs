# NEW_GROUP_REQUEST を映像キーフレーム要求に配線する

- Created: 2026-09-18
- Completed: 2026-10-14
- Branch: feature/add-new-group-request-keyframe
- Polished: 2026-09-28
- Updated: 2026-10-10

## 目的

draft-ietf-moq-transport-22 §3.5.1 / §9.20.19 の `NEW_GROUP_REQUEST` は、購読者が Group の途中で参加したときに新しい Group (= ランダムアクセス点) を要求する仕組みである。現在の publisher example は要求に `REQUEST_OK` を返すだけで、キーフレーム生成に結び付けていないため、購読者は次の周期キーフレームまで待たされる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `run` の `ClientEvent::RequestUpdate` 分岐は、受信した `REQUEST_UPDATE` に `REQUEST_OK` を返すだけである (`serve_peer_request` は SUBSCRIBE / FETCH を扱う関数であり、`REQUEST_UPDATE` はここでは扱わない)
- `examples/tokio-moq/src/moqt_client.rs` の `publish_track` は空の `TrackProperties::new()` で PUBLISH するため、`DYNAMIC_GROUPS` (draft-ietf-moq-transport-22 §10.6) を告知できない
- `ClientEvent::RequestUpdate` の `IncomingRequestUpdate` は `parameters: MessageParameters` を公開しており、`MessageParameters::new_group_request()` (draft-ietf-moq-transport-22 §9.20.19 のパラメータ型 0x32) で `NEW_GROUP_REQUEST` の有無と値を example 側で判定できる
- session 層は `NEW_GROUP_REQUEST` と `DYNAMIC_GROUPS` の検証を実装済みである。受信側は `src/session/subscription/recv.rs` の
  `handle_update_for_subscription` が `DYNAMIC_GROUPS=1` でない Track への `NEW_GROUP_REQUEST` を、送信側は
  `src/session/subscription/send.rs` の `send_request_update` が同条件を拒否する。`DYNAMIC_GROUPS` の値域 (0 / 1 のみ) は
  `src/track_properties.rs` の `validate_track_property_value_range` が encode / decode の両方で検証する
- 任意フレームでキーフレームを要求する経路は 0102 で追加する。0102 は未完了であり、本 issue の実装は 0102 の完了を前提とする
- `--input-mp4` (パススルー) は `examples/moq-pub/src/pipeline.rs` でエンコーダを生成せずエンコード済みサンプルをそのまま送るため、Group は MP4 のキーフレームで進み、キーフレーム強制の経路を通らない (0179 / 0180 で追加された経路)

## 設計方針

- 映像 Track の PUBLISH で `DYNAMIC_GROUPS=1` を告知する。`publish_track` に `TrackProperties` を渡せるようにし、映像 Track だけ `TrackProperty { prop_type: PROP_DYNAMIC_GROUPS, value: TrackPropertyValue::VarInt(1) }` を載せる (音声 / カタログ Track は空のまま)。告知は Original Publisher としての PUBLISH で行い、SUBSCRIBE_OK の Track Properties は変更しない
- `NEW_GROUP_REQUEST` の値が 0 または現在の映像 Group ID より大きい場合にだけ、次の入力フレームで強制キーフレームを要求する (draft-ietf-moq-transport-22 §9.20.19)。値が現在の Group 以下なら何もせず、`REQUEST_OK` は従来どおり返す。次の Group ID はパラメータ値と一致させる必要はない (同 §9.20.19)
- 最小間隔 (例 300 ms) を設け、連続要求でキーフレームが乱発されないようにする。最小間隔内の要求は破棄せず保留し、間隔経過後の最初の入力フレームで強制キーフレームを要求する (draft-ietf-moq-transport-22 §9.20.19 が "The Original Publisher MAY delay the NEW_GROUP_REQUEST subject to implementation specific concerns, for example, achieving a minimum duration for each Group." と定める遅延)
- `ClientEvent::RequestUpdate` で `new_group_request()` が `Some` になるのは session 層の検証を通過した要求だけであり、`DYNAMIC_GROUPS=1` を告知した映像 Track に限られるため、対象 Track の判定は値の確認だけで足りる
- 強制キーフレーム要求は 0102 で追加される `encode()` のキーフレーム強制引数への呼び出し 1 つで実現できる形にする (エンコーダを通らない `--input-mp4` のパススルー経路は対象外)

## 完了条件

- `DYNAMIC_GROUPS=1` を告知した映像 Track への `NEW_GROUP_REQUEST` で、要求後に映像 Group が開始されること
- 最小間隔内の連続要求でキーフレームが乱発されないこと
- 既存の `REQUEST_UPDATE` (Forward / Range Filter) の応答挙動が変わらないこと
- 0102 のキーフレーム強制経路を使っていること

## 解決方法

映像トラックの PUBLISH で `DYNAMIC_GROUPS=1` を告知し、購読者からの `NEW_GROUP_REQUEST` を
映像のキーフレーム要求へ配線した。Group の途中で参加した購読者は、次の周期キーフレームを
待たずに新しい Group の開始を要求できる。

### 変更内容

- `examples/moq-pub/src/pipeline.rs`: 映像トラックの PUBLISH に `DYNAMIC_GROUPS=1` を載せる
  (`video_publish_properties()`。MP4 パススルーでは載せない)。`ClientEvent::RequestUpdate` と
  peer からの SUBSCRIBE の両経路で `NEW_GROUP_REQUEST` を `accepted_new_group_request()` で
  判定し、購読が成立した後に `GopTracker::request_new_group()` を呼ぶ。Group を開始した出力
  フレームを観測して writer を作れた時点で `started_video_group_id` を記録する
- `examples/moq-pub/src/gop.rs`: `accepts_new_group_request()` (値の受け付け判定) と
  `GopTracker::request_new_group()` を追加する。直前の Group が最小長
  (`MIN_GROUP_DURATION_US` = 300 ms) に達するまでは新しい Group を前倒しせず、保留した要求は
  キーフレームの観測で満たされたものとして消す
- `examples/tokio-moq/src/moqt_client.rs`: `publish_track()` に Track Properties を渡せるようにする
- `examples/README.md`: `DYNAMIC_GROUPS=1` の告知と `NEW_GROUP_REQUEST` の扱いを追記する

### 判断と根拠

- **告知は PUBLISH の Track Properties で行う**: §9.20.19 は「Track が DYNAMIC_GROUPS を 1 で
  持たない限り、購読者は REQUEST_UPDATE で NEW_GROUP_REQUEST を送ってはならない (MUST NOT)」
  と定める。Original Publisher としての PUBLISH で告知し、SUBSCRIBE_OK の Track Properties は
  変更しない (§3.5.1 が示す購読パターンは PUBLISH 起点の購読を対象とする)
- **MP4 パススルーでは告知も受理もしない**: §10.6 は値 1 を「購読者が新しい Group の開始を
  要求できる」ことの告知と定める。パススルーはエンコーダを使わずキーフレームを要求できない
  ため、できるように見せる告知をしない
- **値が 0 か現在の Group より大きいときだけ受け付ける**: 同 §9.20.19 の SHOULD の条件と
  一致する。値は購読者が知っている最大の Group ID + 1 であり、現在の Group 以下なら新しい
  Group を開始する理由がない。まだ Group を開始していない間は値によらず受け付ける
- **REQUEST_UPDATE は映像の購読に限る**: 要求は Track 単位であるため、他のトラックの要求で
  映像のキーフレームを強制しない (session 層も DYNAMIC_GROUPS を告知していない購読の要求は
  プロトコル違反として拒否する)
- **最小長 300 ms は前倒しを遅らせる門である**: 同 §9.20.19 は Group 長の最小値を実現する
  などの理由で要求を遅延させてよい (MAY) と定める。保留した要求は破棄せず、最小長に達した
  最初の入力フレームでキーフレームを要求する。`--gop-duration` による通常の境界が先に来た
  場合はそこで Group が始まり、要求は満たされる
- **キーフレームの観測で要求を消す**: 新しい Group が始まれば要求は満たされている。残すと
  最小長の経過後に誰も要求していないキーフレームを強制し、短い Group が 1 つ増える
- **Group ID は要求の値と一致させない**: 同 §9.20.19 が publisher が選ぶと定めているため、
  既存の Group ID の払い出し (Unix epoch ミリ秒) をそのまま使う
- **`NEW_GROUP_REQUEST` の受理は購読が成立した後に行う**: 応答 (SUBSCRIBE_OK / REQUEST_OK) が
  peer の stream reset で失敗した場合は購読が成立していないため、キーフレームを要求しない

### テスト

- `examples/moq-pub/src/gop.rs`: 値の受け付け判定 (0 / 現在と同じ / 過去 / 未来 / Group 未開始)、
  最小長の経過後と 1 マイクロ秒手前、連続要求でキーフレームが乱発しないこと、キーフレームの
  観測で要求が満たされること、Group を開始する前に要求しないこと
- `examples/moq-pub/src/pipeline.rs`: 映像トラックに `DYNAMIC_GROUPS=1` を載せ、MP4 パススルー
  では載せないこと、`NEW_GROUP_REQUEST` を映像トラックの要求としてだけ受け付けること
- `examples/tokio-moq/src/peer_harness.rs`: PUBLISH に載せた Track Properties が peer へ届くこと

### スコープ外 (別 issue)

- `NEW_GROUP_REQUEST` の実 relay での往復確認は、この repo の購読側 (moq-sub) が
  `NEW_GROUP_REQUEST` を送らないため未実施である
- `pipeline::run` と `GopTracker` の結線は単体テストで守られていない
- peer からの SUBSCRIBE で確立した購読に `DYNAMIC_GROUPS` を告知しないため、その購読で
  REQUEST_UPDATE に `NEW_GROUP_REQUEST` を載せると session 層がプロトコル違反として接続を閉じる

### 確認

- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る
- 実 relay で `moq-pub` の PUBLISH (映像 / カタログ) が通り、PUBLISH 後に relay から
  REQUEST_UPDATE が届くことを確認した
- `e2e-tests` の publish / subscribe 往復が実 relay で通る
