# PUBLISH の FILL_PARAMETERS から初回 fill を開設する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.4、§3.4.1、§9.8、§9.19.15](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` の `PUBLISH_ALLOWED_PARAMS` に FILL_PARAMETERS がなく、`src/session/subscription/fill.rs` の開設処理は SUBSCRIBE / REQUEST_UPDATE を中心に呼ばれる。
`src/session/core.rs` の `range_filter_usage` の説明も送信元をこの 2 種に限っている。
`src/session/data.rs` の `Session::recv_fetch_header` は未登録 Request ID でセッションを終了する。既存の `PendingSubgroupBuffer` は Track Alias を使う subgroup 専用であり、先着 fill にそのまま適用できない。

## 設計方針

PUBLISH でも FILL_PARAMETERS を受理・送信し、publisher が PUBLISH を送るときに unpaused なら初回 fill を開く。
初回 FETCH_HEADER は subscription の Request ID を使う。
LARGEST_OBJECT、INCLUDE_PROPERTIES、フィルタと優先度、内側 Range の会計を既存 fill と整合させる。
PUBLISH_OK を送る subscriber が誤って fill を送信しないよう、起点と役割を明確にする。
先着する初回 fill について、`Session::recv_fetch_header` は対応 PUBLISH が未登録であることを再試行可能な結果で返し、その時点ではセッションを終了しない。
`crates/tokio-moq/src/moqt_client.rs` の `DataPlaneHandle::recv_fetch_header` から受信側へ伝え、Request ID をキーに stream と decode 済みヘッダを上限・期限付きで保留する。
PUBLISH 受理後にヘッダを Session へ再通知し、受理されてから entry 処理へ進める。保留の上限・期限超過時は stream を破棄して会計を解放する。
`crates/moq-sub/src/pipeline.rs` の `handle_stream_body` / `fetch_stream_action` も変更対象とし、既存の `fetch_targets` にない Request ID を読み捨てる判定で PUBLISH 起因の fill を失わないようにする。

## 完了条件

- PUBLISH の fill が送信時に 1 本開設され、paused・FILL_PARAMETERS 省略では開設されないこと。
- PUBLISH より先着した fill がセッション終了や読み捨てにならず、有限保留から PUBLISH 受理後に再投入されること。保留の上限・期限超過時に stream と会計が残らないこと。
- 初回と REQUEST_UPDATE 起因の Request ID、Range 上限、プロパティの条件を検証すること。

## 依存・関連

- 先行: [0380](0380-moqt-draft-23-fill-parameters-count.md)
- 先行: [0381](0381-moqt-draft-23-paused-parameter.md)

## 解決方法

`PUBLISH_ALLOWED_PARAMS` に FILL_PARAMETERS を追加し、`Publish::encode_message_body` /
`Publish::decode_message_body` / `send_publish` / `accept_peer_request` のスコープ検証で
PUBLISH の FILL_PARAMETERS を受理・送信できるようにした (§9.8 / §9.19.15)。

- `send_publish` は送信前に `validate_outgoing_fill_parameters` で FILL 内側の Table 7 スコープと
  LOCATION_FILTER を検証し、PUBLISH の `SendRequest` を積んだ後に `maybe_open_fill_stream` で初回 fill を開く。
  unpaused (PAUSED=0) かつ Largest Object が既知で fill range が空でない場合だけ開く (§3.4 / §3.4.1)。
  初回 fill の FETCH_HEADER は PUBLISH の Request ID (= subscription の Request ID) を載せ、
  REQUEST_UPDATE 起因の fill が自身の Request ID を使う既存動作と整合させた (§3.4 / §6.4.2.1)。
  LARGEST_OBJECT は `publisher_track_largest` の Track 単位の値で解決し、フィルタと優先度は
  PUBLISH の初期 subscription パラメータを `filter_view` として渡して既存 fill と同じ評価にした。
  FILL 内側の Range は `range_filter_usage` が従来から内側を合算しており、PUBLISH でも同じ予算に計上される (§9.1.6 / §3.3.2)。
- `maybe_open_fill_stream` は外側 `MessageParameters` ではなく FILL 内側パラメータを受け取る形に変え、
  開設主体が publisher であることと PUBLISH が起点になり得ることを doc に明記した。
  PUBLISH を受けて PUBLISH_OK を返す subscriber は `handle_peer_publish` のロール判定と doc により開設しない。
- `Session::recv_fetch_header` の戻り値を `FetchHeaderAcceptance` に変え、未知の Request ID では
  `RequestNotRegistered` を返して状態を変更せずセッションも閉じない (§11.4.1 は未知 ID の扱いを MUST で定めない)。
- `crates/tokio-moq/src/moqt_client.rs` の `DataPlaneHandle::recv_fetch_header` は、
  `RequestNotRegistered` を受けると Request ID をキーに decode 済みヘッダを上限 64 件・期限 5 秒で保留し、
  `FetchHeaderRegistration::Pending` を返す。stream 自体は受信タスクが保持したまま `wait_fetch_header_registration`
  で待機し、保留中は entry の読み取りも会計も進めない (async な stream を共有状態へ移さずに同じ効果を得る)。
  `MoqtClient::pump_once` が PUBLISH を受理した直後に保持中のヘッダを Session へ再通知し、受理されたら
  待機側へ `FetchHeaderResolution::Accepted` を渡す。期限・上限超過、および PUBLISH が受理されなかった場合は
  Session の会計を解放してから破棄する。
- `crates/moq-sub/src/pipeline.rs` の `handle_stream_body` は `fetch_targets` による配送先判定より先に
  `recv_fetch_header` へ登録するようにし、保留の待機 (`register_fetch_header`) を挟んだ。
  これにより `fetch_targets` に現れない PUBLISH 起点の初回 fill を Session へ届けずに失わない。
  `fetch_stream_action` の `Ignore` は「このアプリに配送先が無い」ことだけを意味するよう doc を更新した。
- `src/session/core.rs` の `range_filter_usage` の説明を SUBSCRIBE / REQUEST_UPDATE / PUBLISH を送信元とする形に更新した。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| PUBLISH の fill が送信時に 1 本だけ開設される | `tests/test_session/fetch/fill.rs` の `publish_with_fill_opens_initial_fill_stream` と `pbt/tests/prop_session/subscription.rs` の `publish_fill_opens_only_when_unpaused_and_largest_known`。イベント順序 (PUBLISH の後に開設) と 1 本であることを固定 |
| paused・FILL_PARAMETERS 省略では開設されない | `publish_with_fill_and_paused_opens_nothing` / `publish_without_fill_opens_nothing` / `publish_with_fill_and_no_largest_object_opens_nothing` と上記 PBT |
| 先着した初回 fill がセッション終了・読み捨てにならない | `fill_fetch_header_with_unknown_request_id_is_retryable` / `fetch_header_is_accepted_after_publish_arrives` / `subscriber_receives_publish_with_fill_and_opens_nothing` と PBT の `publish_initial_fill_is_accepted_in_either_arrival_order` |
| 保留が有限で、上限・期限超過時に stream と会計が残らない | `crates/tokio-moq/src/moqt_client.rs` の `pending_fetch_header_expires_and_releases_accounting` / `pending_fetch_headers_are_capped` / `pending_fetch_header_is_discarded_when_publish_is_not_accepted` |
| 初回と REQUEST_UPDATE 起因の Request ID | `initial_and_update_fill_use_distinct_request_ids` と上記 PBT (初回 fill の Request ID が PUBLISH の Request ID であること) |
| Range 上限 | `publish_with_fill_opens_initial_fill_stream` (fill range 外の Object が `LocalFilterMismatch` になること) と `request_update` 系の既存テスト |
| プロパティの条件 | `publish_with_include_properties_is_rejected_without_side_effects`。INCLUDE_PROPERTIES は §9.8 の PUBLISH パラメータにも FILL 内側の Table 7 にも無いため、PUBLISH では送信前に拒否し fill を開かない。fill で送る Object の Properties の取捨は I/O 層 (publisher アプリ) の責務であり、Session の `send_fetch_object` は Location 検証だけを行う |
| 内側 Range の会計 | `tests/test_session/request_stream.rs` の `publish_fill_inner_range_filters_count_toward_max_filter_ranges` / `publish_fill_inner_duplicate_range_filters_is_rejected` |
| 検証コマンド | `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` がすべて成功 |
