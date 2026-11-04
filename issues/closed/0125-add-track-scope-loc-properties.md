# Track スコープの LOC プロパティを subscriber に公開する

- Created: 2026-09-21
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-10-25
- Refreshed: 2026-11-04

## 目的

draft-ietf-moq-loc-05 §6.1 (MOQ Properties Registry) の Table 1 は `0x08` (TIMESCALE) / `0x0D` (VIDEO_CONFIG) / `0x0F` (AUDIO_CONFIG) の Scope を "Track, Object" と定める。

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

Scope が Track である値は Track Properties として購読確立時に一度だけ送れる。draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は Track Properties の位置を次のように定める。

> Properties are serialized as Key-Value-Pairs (see Figure 4).
> Track Properties always appear as the final field in the messages that carry them; their length is the remaining bytes of the message after all preceding fields have been consumed.
> Object Properties (Section 11.1.2) are preceded by an explicit length field.

現状の受信側は peer の Track Properties を保持しないため、Track スコープで送られた Video Config / Audio Config / Timescale を捨てる。publisher がこれらを Object Properties として毎回載せない選択をした時点で、codec extradata (H.264/H.265 の parameter set、Opus の OpusHead) と時間軸の単位が application に届かない。

## 現状

- `src/session/types.rs` の `Subscription` は peer の Track Properties そのものを保持せず、`track_properties` から取り出した既知の値だけを個別フィールドとして持つ
- `src/session/subscription/recv.rs` の `handle_peer_publish` は `publish.track_properties` から `has_unknown_mandatory` / `object_delivery_timeout` / `subgroup_delivery_timeout` / `dynamic_groups` / `default_publisher_priority` / `default_publisher_group_order` を取り出し、
  `Subscription` を構築する時点で残りを捨てる。
  `handle_peer_subscribe_ok` も同様に `ok.track_properties` から既知の MOQT 値だけを取り込む
- `src/session/subscription/send.rs` の `send_publish` / `send_subscribe_ok` は `TrackProperties` を引数で受け取って wire に載せる。publisher 役の `Subscription` にも自側の値を個別フィールドとして保持するだけで、受け取った `TrackProperties` そのものは保持しない
- `src/track_properties.rs` の `TrackProperties` は `as_slice()` と `find_varint()` を持ち、奇数型の値を `TrackPropertyValue::Bytes(Vec<u8>)` として保持する。LOC の `0x0D` / `0x0F` は最上位リストに置かれた場合はこの形で生バイト列を取り出せる
- `src/session/subscription.rs` の `Session::subscription` / `Session::subscriptions` は `&Subscription` を返すため、`Subscription` にフィールドを追加すれば application から参照できる
- `src/object_properties.rs` の `ObjectProperties` は MOQT が定義する Object Properties (draft-ietf-moq-transport-23 §16.8 (Properties) Table 15) だけを対象にしており、LOC のプロパティを解釈する口は無い
- `crates/tokio-moq/src/moqt_client.rs` の `publish_track` は呼び出し側が渡した `TrackProperties` を PUBLISH する。
  `crates/moq-pub/src/pipeline.rs` の `publish_video_track` / `publish_audio_track` は `MAX_CACHE_DURATION` を載せ、
  映像のエンコード経路では `DYNAMIC_GROUPS` も載せる。`serve_peer_request` は `subscribe_ok_properties` が返す
  同じ MOQT プロパティで応答するが、LOC の Track スコープ値 (`0x08` / `0x0D` / `0x0F`) は一度も告知しない
- `crates/moq-sub/src/pipeline.rs` は Object ごとに `LocProperties::decode` して `extract_video_config` / `extract_audio_config` / `extract_timestamp_timescale` で取り出しており、Track スコープで送られた値を受け取る経路が無い

## 設計方針

- `Subscription` に peer が送った `TrackProperties` を保持する公開フィールド (`peer_track_properties`) を追加し、
  `handle_peer_publish` と `handle_peer_subscribe_ok` で decode 済みの値をそのまま格納する。
  publisher 役の構築箇所 (`send_publish` / `handle_peer_subscribe`) と `send_subscribe`、テスト用ヘルパでは
  `TrackProperties::new()` で初期化する。application は `Session::subscription(request_id)` の
  `Subscription::peer_track_properties` から `0x08` を `find_varint(0x08)` で、`0x0D` / `0x0F` の生バイト列を
  `find_bytes(0x0D)` / `find_bytes(0x0F)` で取り出す
- `TrackProperties::as_slice()` は最上位リストしか返さず、`IMMUTABLE_PROPERTIES` (0x0B) の内側に置かれた値を取り出せない。§10.7 (Immutable Properties) は "processors MUST search both the mutable properties and the contents of Immutable Properties" と定める
- LOC-05 §3.1.2 も LOC のプロパティを Immutable Properties として符号化することを SHOULD としている。`find_varint` と同じ探索規則 (最上位 → 内側) で生バイト列を返す
  `TrackProperties::find_bytes(&self, prop_type: u64) -> Option<&[u8]>` を追加し、`0x0D` / `0x0F` はこれで取得する。
  `IMMUTABLE_PROPERTIES` の内側から取得する場合も外側の `Bytes` を借用して返し、`Vec` へコピーしない。
  内側が破損している場合は `find_varint` と同じく `None` を返す
- `Subscription` は公開 struct であり、フィールド追加に伴い構築箇所 (`src/session/subscription/recv.rs` / `src/session/subscription/send.rs` / `src/session/types.rs` / `src/session/subscription/delivery.rs`) を更新する
- `Subscription` を直接構築しているのは `src/` の 6 箇所である (`src/session/subscription/recv.rs` の 2 箇所 / `src/session/subscription/send.rs` の 2 箇所 / `src/session/types.rs` / `src/session/subscription/delivery.rs`)。うち本番経路は 4 箇所、テスト用ヘルパは 2 箇所 (`src/session/types.rs` / `src/session/subscription/delivery.rs`) である
- `pbt/tests/prop_session/` は `Subscription` を直接構築しないため影響を受けない (`pbt` は workspace member であり `make test` の対象である)
- 既知の MOQT 値 (DYNAMIC_GROUPS / DEFAULT_PUBLISHER_PRIORITY / DEFAULT_PUBLISHER_GROUP_ORDER / delivery timeout) の取り込みは現状のまま維持する。フィールドの追加であり、既存の個別フィールドを置き換えない
- publisher 役 (`send_publish` / `send_subscribe_ok`) が持つ自側の値と混同しないよう、フィールドの意味を「peer が送った Track Properties」に限定して doc コメントに書く
- INCLUDE_PROPERTIES=0 で空化された Track Properties は空のまま保持する (draft-ietf-moq-transport-23 §9.19.22 (INCLUDE_PROPERTIES Parameter) は空化を SHOULD とする)。空化された場合は application が Object Properties 側の値を使う判断をする
- LOC の Track スコープ値をどう解釈するか (Timestamp の単位決定にどう効かせるか) は 0103 (closed、2026-10-06) の
  wall-clock 統一方針と整合させる。0103 は live capture では publisher が `PROP_TIMESCALE` を付けない方針を
  確定させている (`--input-mp4` / `--input-mp4-reencode` は入力ファイルのメディア時刻を送るため入力トラックの
  timescale を付けたままにする)。本 issue は「値を application に届ける」ところまでとし、Timestamp の解釈は変更しない
- publisher example から LOC の Track スコープ値を送る変更は本 issue の対象外とする。受信側の公開 API を先に整える
- FETCH_OK の Track Properties は fetch の応答であって subscription に紐づかないため対象外とする
- Object スコープ側の `LocProperties` の IMMUTABLE_PROPERTIES 探索は 0309 の射程であり、本 issue では扱わない

## 完了条件

- Track スコープで `0x0D` (Video Config) / `0x0F` (Audio Config) / `0x08` (Timescale) を載せた Track Properties を送る peer から、subscriber 役の `Session` の `Session::subscription(request_id)` 経由で application がそれらの値を取得できることを固定するテストが `tests/test_session/` に追加されていること。PUBLISH と SUBSCRIBE_OK の両方の経路を対象にする。
  `tests/test_session/` は integration test であり `pub(crate)` なハンドラは呼べないため、公開入口の `Session::recv_request` (peer publisher → 自側 subscriber) と `Session::recv_stream_message` (自側 subscriber が peer publisher の SUBSCRIBE_OK を受ける) を経由させる
- `0x0D` / `0x0F` が `TrackPropertyValue::Bytes` の生バイト列として、`0x08` が varint として取り出せること。`0x0D` / `0x0F` と `0x08` が最上位リストに置かれた場合と `IMMUTABLE_PROPERTIES` の内側に置かれた場合の両方で取り出せること
- 既知の MOQT 値 (DYNAMIC_GROUPS / DEFAULT_PUBLISHER_PRIORITY / DEFAULT_PUBLISHER_GROUP_ORDER / OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT) の取り込み結果が変わらないこと
- peer が Track Properties を送らない場合と INCLUDE_PROPERTIES=0 で空化された場合に、空の `TrackProperties` として取得でき panic しないこと
- publisher 役 (`send_publish` / `send_subscribe_ok`) と `send_subscribe` 直後の `Subscription` では `peer_track_properties` が空であること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

## 解決方法

`Subscription` に peer が送った Track Properties を保持する公開フィールドを追加し、
`TrackProperties` に IMMUTABLE_PROPERTIES の内側も探索する `find_bytes` を追加した。

- `src/session/types.rs` の `Subscription` に `peer_track_properties` を追加した。peer が
  PUBLISH / SUBSCRIBE_OK で送った Track Properties を加工せずに保持し、自側が publisher として
  送った値は個別の既知値フィールドにのみ反映する
- `src/session/subscription/recv.rs` の `handle_peer_publish` / `handle_peer_subscribe_ok` で
  受信値を格納し、`src/session/subscription/send.rs` / `src/session/subscription/delivery.rs` /
  `src/session/types.rs` の Subscription 構築箇所を空初期化に揃えた
- `src/track_properties.rs` に `find_bytes(&self, prop_type: u64) -> Option<&[u8]>` を追加した。
  `find_varint` と共通の内部探索を使い、mutable リストを先に、次に IMMUTABLE_PROPERTIES (0x0B)
  の内側を探索する。内側の値は外側のバイト列を借用したまま返す
- KVP の借用走査 (`walk_track_kv_pairs`) と所有値への変換 (`decode_track_kv_pairs`) を共通化し、
  専用パーサーを増やさずに両方を提供した
- `tests/test_session/peer_track_properties.rs` に、wire の encode / decode を経由した
  PUBLISH / SUBSCRIBE_OK の両経路で 0x08 / 0x0D / 0x0F を最上位と IMMUTABLE_PROPERTIES の内側から
  取得できること、空と INCLUDE_PROPERTIES=0、自側構築時の空、既知 MOQT 値の非退行、
  未知 Property の保持を検証するテストを追加した
- `tests/test_track_properties.rs` に `find_bytes` の探索規則・空値と欠落の区別・破損した
  IMMUTABLE 内側・値形式の読み飛ばしのテストを追加し、`pbt/tests/prop_track_properties.rs` に
  未知型の探索規則の PBT を追加した
- `skills/shiguredo-moqt/SKILL.md` の利用者向け API 一覧に `find_bytes` と
  `Subscription::peer_track_properties` の説明を追加し、`docs/missing-features.md` の
  Track スコープ LOC Property の記述を実装に合わせて更新した

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| PUBLISH と SUBSCRIBE_OK の両経路で application が track スコープ値を取得できること | `tests/test_session/peer_track_properties.rs` の `publish_exposes_track_scope_loc_properties` / `subscribe_ok_exposes_track_scope_loc_properties` (wire encode / decode 経由) |
| 0x0D / 0x0F が生バイト列、0x08 が varint として、最上位と IMMUTABLE 内側の両方で取得できること | 同ファイルの `*_in_immutable` 2 件と `assert_loc_track_properties` (型の取り違えで None になることも確認) |
| 既知 MOQT 値の取り込みが変わらないこと | 同ファイルの `publish_keeps_known_moq_values_with_loc_properties` / `subscribe_ok_keeps_known_moq_values_with_loc_properties` と既存の default_publisher_properties / delivery timeout テスト |
| peer が Track Properties を送らない場合と INCLUDE_PROPERTIES=0 で空化された場合に空として取得できること | 同ファイルの `*_without_track_properties_*` / `subscribe_ok_with_include_properties_zero_*` |
| publisher 役と send_subscribe 直後の Subscription では空であること | 同ファイルの `local_side_subscriptions_have_empty_peer_track_properties` |
| `make test` / `make clippy` / `make fmt` が通ること | Linux 環境では workspace ビルドが libpulse 依存で失敗するため、libpulse を展開した代替環境で fmt / clippy / test / rustdoc を実行し、すべて成功した |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test --workspace` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` は
すべて成功した。
