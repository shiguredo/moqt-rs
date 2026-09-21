# Track スコープの LOC プロパティを subscriber に公開する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/add-track-scope-loc-properties
- Polished: 2026-09-21

## 目的

draft-ietf-moq-loc-04 §6.1 (MOQ Properties Registry) の Table 1 は `0x08` (TIMESCALE) / `0x0D` (VIDEO_CONFIG) / `0x0F` (AUDIO_CONFIG) の Scope を "Track, Object" と定める。

> ```text
> +======+=====================+===============+=================+
> | Type | Name                | Scope         | Specification   |
> +======+=====================+===============+=================+
> | 0x10 | TIMESTAMP           | Object        | Section 2.3.1.1 |
> +------+---------------------+---------------+-----------------+
> | 0x08 | TIMESCALE           | Track, Object | Section 2.3.1.2 |
> +------+---------------------+---------------+-----------------+
> | 0x09 | VIDEO_FRAME_MARKING | Object        | Section 2.3.2.2 |
> +------+---------------------+---------------+-----------------+
> | 0x0C | AUDIO_LEVEL         | Object        | Section 2.3.3.2 |
> +------+---------------------+---------------+-----------------+
> | 0x0D | VIDEO_CONFIG        | Track, Object | Section 2.3.2.1 |
> +------+---------------------+---------------+-----------------+
> | 0x0F | AUDIO_CONFIG        | Track, Object | Section 2.3.3.1 |
> +------+---------------------+---------------+-----------------+
>
>                                 Table 1
> ```

Scope が Track である値は Track Properties として購読確立時に一度だけ送れる。draft-ietf-moq-transport-21 §8.4 (Track and Object Properties) は Track Properties の位置を次のように定める。

> Properties are serialized as Key-Value-Pairs (see Figure 2).
> Track Properties always appear as the final field in the messages that carry them; their length is the remaining bytes of the message after all preceding fields have been consumed.
> Object Properties (Section 11.1.3) are preceded by an explicit length field.

現状の受信側は peer の Track Properties を保持しないため、Track スコープで送られた Video Config / Audio Config / Timescale を捨てる。publisher がこれらを Object Properties として毎回載せない選択をした時点で、codec extradata (H.264/H.265 の parameter set、Opus の OpusHead) と時間軸の単位が application に届かない。

## 現状

- `src/session/types.rs` の `Subscription` は peer の Track Properties を保持せず、取り込み済みの `dynamic_groups` / `default_publisher_priority` / `default_publisher_group_order` / `delivery_timeouts` だけを持つ
- `src/session/subscription/recv.rs` の `handle_peer_publish` は `publish.track_properties` から `has_unknown_mandatory` / `object_delivery_timeout` / `subgroup_delivery_timeout` / `dynamic_groups` / `default_publisher_priority` / `default_publisher_group_order` を取り出し、
  `Subscription` を構築する時点で残りを捨てる。
  `handle_peer_subscribe_ok` も同様に `ok.track_properties` から既知の MOQT 値だけを取り込む
- `src/session/subscription/send.rs` の `send_publish` / `send_subscribe_ok` は `TrackProperties` を引数で受け取って wire に載せる。publisher 役の `Subscription` にも自側の値を個別フィールドとして保持するだけで、受け取った `TrackProperties` そのものは保持しない
- `src/track_properties.rs` の `TrackProperties` は `as_slice()` と `find_varint()` を持ち、奇数型の値を `TrackPropertyValue::Bytes(Vec<u8>)` として保持する。LOC の `0x0D` / `0x0F` はこの形で生バイト列を取り出せる
- `src/session/subscription.rs` の `Session::subscription` / `Session::subscriptions` は `&Subscription` を返すため、`Subscription` にフィールドを追加すれば application から参照できる
- `src/object_properties.rs` の `ObjectProperties` は MOQT が定義する Object Properties (draft-ietf-moq-transport-21 §16.8 (Properties) Table 14) だけを対象にしており、LOC のプロパティを解釈する口は無い
- `examples/moqt-transport/src/moqt_client.rs` の `publish_track` は `TrackProperties::new()` で PUBLISH し、`examples/moqt-publisher/src/pipeline.rs` の `serve_peer_request` は `send_subscribe_ok(..., TrackProperties::new())` で応答する。publisher example は LOC の Track スコープ値を一度も告知しない
- `examples/moqt-subscriber/src/pipeline.rs` は Object ごとに `LocProperties::decode` して `extract_video_config` / `extract_audio_config` / `extract_timestamp_timescale` で取り出しており、Track スコープで送られた値を受け取る経路が無い

## 設計方針

- `Subscription` に peer が送った `TrackProperties` を保持するフィールド (`peer_track_properties`) を追加し、`handle_peer_publish` と `handle_peer_subscribe_ok` で decode 済みの値をそのまま格納する。application は `Session::subscription(request_id)` から `0x08` を `find_varint()` で、`0x0D` / `0x0F` の生バイト列を新しい accessor で取り出す
- `TrackProperties::as_slice()` は最上位リストしか返さず、`IMMUTABLE_PROPERTIES` (0x0B) の内側に置かれた値を取り出せない。§10.7 (Immutable Properties) は "processors MUST search both the mutable properties and the contents of Immutable" と定める
- LOC-04 §3.1.2 も LOC のプロパティを Immutable Properties として符号化するとしている。`find_varint` と同じ探索規則 (最上位 → 内側) で生バイト列を返す `TrackProperties::find_bytes` を追加し、`0x0D` / `0x0F` はこれで取得する
- `Subscription` は公開 struct であり、フィールド追加に伴い構築箇所 (`src/session/subscription/recv.rs` / `src/session/subscription/send.rs` / `src/session/types.rs` / `src/session/subscription/delivery.rs`) を更新する
- `Subscription` を直接構築しているのは `src/` の 6 箇所だけである (`src/session/subscription/recv.rs` の 2 箇所 / `src/session/subscription/send.rs` の 2 箇所 / `src/session/types.rs` / `src/session/subscription/delivery.rs`)
- `pbt/tests/prop_session/` は `Subscription` を直接構築しないため影響を受けない (`pbt` は workspace member であり `make test` の対象である)
- 既知の MOQT 値 (DYNAMIC_GROUPS / DEFAULT_PUBLISHER_PRIORITY / DEFAULT_PUBLISHER_GROUP_ORDER / delivery timeout) の取り込みは現状のまま維持する。フィールドの追加であり、既存の個別フィールドを置き換えない
- publisher 役 (`send_publish` / `send_subscribe_ok`) が持つ自側の値と混同しないよう、フィールドの意味を「peer が送った Track Properties」に限定して doc コメントに書く
- INCLUDE_PROPERTIES=0 で空化された Track Properties は空のまま保持する (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter) は空化を SHOULD とする)。空化された場合は application が Object Properties 側の値を使う判断をする
- LOC の Track スコープ値をどう解釈するか (Timestamp の単位決定にどう効かせるか) は [issues/0103](../issues/0103-change-loc-timestamp-wall-clock.md) の wall-clock 統一方針と整合させる。0103 は publisher が `PROP_TIMESCALE` を付けない方針のため、本 issue は「値を application に届ける」ところまでとし、Timestamp の解釈は変更しない
- publisher example から LOC の Track スコープ値を送る変更は本 issue の対象外とする。受信側の公開 API を先に整える
- `CHANGES.md` の `## develop` に `[ADD]` エントリを追加する (`Subscription` への公開フィールド追加と `TrackProperties::find_bytes` の追加)
- FETCH_OK の Track Properties は fetch の応答であって subscription に紐づかないため対象外とする

## 完了条件

- Track スコープで `0x0D` (Video Config) / `0x0F` (Audio Config) / `0x08` (Timescale) を載せた Track Properties を送る peer から、subscriber 役の `Session` の `Session::subscription(request_id)` 経由で application がそれらの値を取得できることを固定するテストが `tests/test_session/` に追加されていること。
  `tests/test_session/` は integration test であり `pub(crate)` なハンドラは呼べないため、公開入口の `Session::recv_request` (peer publisher → 自側 subscriber) と `Session::recv_stream_message` (自側 subscriber が peer publisher の SUBSCRIBE_OK を受ける) を経由させる
- `0x0D` / `0x0F` が `TrackPropertyValue::Bytes` の生バイト列として、`0x08` が varint として取り出せること。`0x0D` / `0x0F` が最上位リストに置かれた場合と `IMMUTABLE_PROPERTIES` の内側に置かれた場合の両方で取り出せること
- 既知の MOQT 値 (DYNAMIC_GROUPS / DEFAULT_PUBLISHER_PRIORITY / DEFAULT_PUBLISHER_GROUP_ORDER / OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT) の取り込み結果が変わらないこと
- peer が Track Properties を送らない場合と INCLUDE_PROPERTIES=0 で空化された場合に、空の `TrackProperties` として取得でき panic しないこと
- `CHANGES.md` の `## develop` に `[ADD]` エントリが追加されていること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること
