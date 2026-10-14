# 購読経路でも動的 Group を告知し subscriber から新しい Group を要求できるようにする

- Created: 2026-10-14
- Completed: 2026-10-14
- Branch: feature/add-subscriber-new-group-request
- Polished: {YYYY-MM-DD}

## 目的

Group の途中で参加した購読者は、次の周期キーフレームまで復号を始められない (既定の
`--gop-duration` では 2000 ms)。publisher は映像トラックで `NEW_GROUP_REQUEST`
(draft-ietf-moq-transport-22 §9.20.19) を受け付けるようにしたが、告知が PUBLISH の
Track Properties に限られているため購読経路によっては要求できない。また subscriber
example (moq-sub) は購読時に要求を送らないため、この経路が実 relay で確認できていない。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` は SUBSCRIBE_OK に空の
  Track Properties を渡す。session 層は SUBSCRIBE_OK の Track Properties から購読の
  `dynamic_groups` を決めるため、peer の SUBSCRIBE で確立した購読は `false` になり、
  その購読で REQUEST_UPDATE に `NEW_GROUP_REQUEST` を載せると §9.20.19 の MUST NOT として
  セッションがプロトコル違反で閉じられる
- `examples/moq-sub/src/pipeline.rs` は `MoqtClient::subscribe_track()` で SUBSCRIBE する。
  パラメータを渡すには `subscribe_track_with_parameters()` を使う
- §10.6 は `DYNAMIC_GROUPS` を Track Property と定め、§3.5.1 は SUBSCRIBE_OK の値で
  購読者が publisher の対応を知ると定める。§9.20.19 は告知が無くても SUBSCRIBE に
  `NEW_GROUP_REQUEST` を載せてよい (MAY) とも定める

## 設計方針

- moq-pub: 映像トラックの SUBSCRIBE_OK に `DYNAMIC_GROUPS=1` を載せ、PUBLISH と同じ告知を
  行う。MP4 パススルーはキーフレームを要求できないため載せない (`video_publish_properties()`
  を PUBLISH と共用する)
- moq-sub: 映像トラックの SUBSCRIBE に `NEW_GROUP_REQUEST=0` を載せる。0 は「購読者が
  Group の情報を持たない」ことを示し、publisher はできるだけ早く新しい Group を開始する
  (§9.20.19)。カタログと音声の購読には載せない
- Group ID は publisher が選ぶため、要求の値と一致させる必要はない (同節)
- 購読の確立後に届く Group から復号する既存の挙動は変えない

## 完了条件

- 実 relay で moq-sub の購読により publisher が新しい Group を開始すること
  (publisher が要求を受理したログで確認する)
- peer の SUBSCRIBE で確立した購読でも REQUEST_UPDATE の `NEW_GROUP_REQUEST` が
  プロトコル違反にならないこと
- カタログと音声の購読の挙動が変わらないこと

## 解決方法

映像トラックの告知を購読経路にも広げ、subscriber example が購読と同時に新しい Group を
要求するようにした。あわせて REQUEST_UPDATE の受理判定を Track 名で解決する形に直し、
どちらの経路で確立した購読でも要求を受け付けるようにした。

### 変更内容

- `examples/moq-pub/src/pipeline.rs`: SUBSCRIBE_OK に載せる Track Properties を
  `subscribe_ok_properties()` で決め、映像トラックには PUBLISH と同じ `DYNAMIC_GROUPS=1` を
  載せる (MP4 パススルーでは載せない)。REQUEST_UPDATE の `NEW_GROUP_REQUEST` は、
  `MoqtClient::subscription_track_name()` で対象の購読の Track 名を引いて映像トラックの
  ときだけ受け付ける
- `examples/moq-sub/src/pipeline.rs`: 映像トラックの SUBSCRIBE に `NEW_GROUP_REQUEST=0` を
  載せる (`video_subscribe_parameters()`)。カタログと音声の購読は変更しない
- `examples/README.md`: 告知の経路と、どちらの経路で確立した購読でも要求を送れることを追記する

### 判断と根拠

- **SUBSCRIBE_OK にも `DYNAMIC_GROUPS=1` を載せる**: §3.5.1 は「SUBSCRIBE_OK の
  DYNAMIC_GROUPS の値が publisher の対応を示す」と定め、§10.6 は `DYNAMIC_GROUPS` を
  Track Property と定める。PUBLISH だけで告知すると、peer の SUBSCRIBE で確立した購読では
  購読者が §9.20.19 の MUST NOT により REQUEST_UPDATE で要求を送れない
- **受理判定は Track 名で行う**: 購読は自側の PUBLISH と peer の SUBSCRIBE の両方で確立し、
  Request ID の空間が異なる (§6.4.2.1 の parity)。`subscription_track_name()` は fill fetch
  経路で同じ解決に使っており、どちらの購読でも映像トラックかどうかを判定できる
- **subscriber は `NEW_GROUP_REQUEST=0` を SUBSCRIBE に載せる**: 同 §9.20.19 は
  `DYNAMIC_GROUPS` の告知が無くても SUBSCRIBE に載せてよい (MAY) と定め、非対応の publisher は
  無視する。購読の確立と同時に要求できるため、REQUEST_UPDATE を 1 往復余分に使わない
- **Group ID は publisher が選ぶ**: 要求の値 (0) と一致させる必要はない (同節)

### テスト

- `examples/moq-pub/src/pipeline.rs`: `subscribe_ok_properties()` が映像トラックだけ
  `DYNAMIC_GROUPS=1` を告知し、MP4 パススルーと音声 / カタログでは告知しないこと
- `examples/moq-sub/src/pipeline.rs`: `video_subscribe_parameters()` が
  `NEW_GROUP_REQUEST=0` だけを載せること

### スコープ外 (別 issue)

- `pipeline::run` と購読パラメータの結線は単体テストで守られていない (呼び出し側を変えても
  テストが落ちない)。既存の結線テストの課題と同じ性質である
- REQUEST_UPDATE 経路の `NEW_GROUP_REQUEST` は、この repo の example からは送らないため
  実 relay での往復確認は SUBSCRIBE 経路のみである (relay が SUBSCRIBE の要求を
  REQUEST_UPDATE として転送する場合は受理することをログで確認した)

### 確認

- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通る
- 実 relay で `moq-pub` (publisher) と `moq-sub` (subscriber) を接続し、publisher が
  `Received NEW_GROUP_REQUEST: next_group_id=0` を記録して新しい Group を開始すること、
  subscriber が 233 フレームをデコードすることを確認した
- `e2e-tests` の publish / subscribe 往復が実 relay で通る
