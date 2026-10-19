# 送信側 delivery timeout を先頭 Object の Object Property override と送信前チェックに対応させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-sender-delivery-timeout
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §5.2 (Delivery Timeouts and Data Reliability) は、publisher の値について「先頭 Object の Object Property があればそれ、なければ Track Property」とし、subgroup では transport に渡す前に経過時間を検査して超過時に `DELIVERY_TIMEOUT` で stream を reset することを MUST とする。datagram では同じ検査で超過時に datagram を drop することを MUST とする。§5.2 の Object Property override は subgroup の先頭 Object に限られ、datagram には適用されない。また §5.2 は、transport が許す場合に再送前の timeout 検査を SHOULD とする。現状は受信側だけが override を実装し、送信側の subgroup は Track Property しか見ず、送信直前チェックも無い。datagram の送信直前 drop は実装済みである。

## 現状

- `src/session/data.rs` の `send_subgroup_object` は `subscription.delivery_timeouts.effective_object_ms` が `Some` のときだけ `object_delivery_header_complete_ms` を記録し、`properties_bytes` はフィルタ評価にしか使わない。
- `src/session/data.rs` の `close_outgoing_subgroup_stream` は `effective_subgroup_ms` が `Some` のときだけ FIN タイマーを開始し、同じく override を見ない。
- `src/session/subscription/dispatch.rs` の `tick_subscription_timeouts` は `effective_object_ms` / `effective_subgroup_ms` で期限を判定しており、override を反映しない。そのため override と Track Property が異なる場合に誤った期限で reset され得る。
- datagram 送信は `effective_object_ms` / `effective_subgroup_ms` を使う。§5.2 の override は subgroup の先頭 Object に限られるため datagram は変更対象外であり、`send_object_datagram` は送信直前の経過時間検査と `SendRequestError::LocalDatagramTimeout` による drop を既に実装している。
- 受信側は `src/session/data.rs` の `subgroup_effective_delivery_timeout` と `subgroup_overrides` で override を実装しており、送信側だけが非対称である。
- 送信側の期限判定は tick 時の処理に限られ、`send_subgroup_object` は「transport に渡す前の検査」を行っていない。datagram は上記のとおり検査済みである。
- 影響: delivery timeout を設定した publisher で override が無視され、期限切れ Object が transport に渡り得る。現在の example は timeout を設定しないため表面化していない。

## 設計方針

- 送信側でも先頭 Object の Object Properties を `decode_object_properties` で decode し、(group_id, subgroup_id) 単位の override を `subscription.delivery_timeouts.subgroup_overrides` に保持する。override はフィルタを通過して実際に送信する Object の Properties からのみ登録し、受信側の `accept_subgroup_object` と同じく送信済み Object にのみ適用する。削除のタイミングは FIN タイマーの参照期間を考慮して後述のとおりとする。`subgroup_effective_delivery_timeout` は publisher 側でも使えるよう doc を更新する。
- 先頭 Object の判定は stream 単位で行う。FirstObjectId モードは `subgroup_id` 解決後の最初の送信 Object、Explicit / Zero モードは `OutgoingDataStream` に追加する「先頭 Object 送信済み」フラグで判定する。
- effective 値は既存の `compute_effective_delivery_timeout_ms` で計算する。0 は timeout なしとして正規化し、購読者値と publisher 値 (override または Track Property) がともに non-zero なら小さい方を使う。
- `send_subgroup_object` は、フィルタ通過と FirstObjectId 解決の後に、最大位置の更新と `object_delivery_header_complete_ms` の記録より前に、当該 object_id の既存の `object_delivery_header_complete_ms` エントリ (再送時) を検査する。先頭 Object の override 登録と「先頭 Object 送信済み」フラグの更新は、この検査を通った後に行う。
- sans-I/O の制約により、初回送信は `send_subgroup_object` 呼び出し時刻を Object header 提供完了時刻の近似とするため経過 0 となり、reset されない。再送で override 込みの effective OBJECT_DELIVERY_TIMEOUT 以上なら、`ResetDataStream` (`STREAM_DELIVERY_TIMEOUT`, `reliable_size: None`) を push し、`SendRequestError::LocalSubgroupTimeout` を返して Object を送らない。最大位置も更新しない。検査を通った場合は、override 込みの effective OBJECT_DELIVERY_TIMEOUT が `Some` なら `object_delivery_header_complete_ms` を記録する (Track-level の `effective_object_ms` が `None` でも override で `Some` になる場合は記録する)。既存エントリがある場合は更新せず、最初の提供完了時刻を保持する。
- 送信直前 reset では stream 追跡を保持し、I/O 層が reset 後に `send_data_stream_closed` で終端通知できる契約を維持する (`reset_outgoing_data_stream_at` は追跡を除去するため使わない)。重複 reset を避けるため、当該 stream の timing エントリは除去する。
- `close_outgoing_subgroup_stream` は FIN 時に override 込みの effective SUBGROUP_DELIVERY_TIMEOUT が `Some` なら FIN タイマーを開始する。stream は `data_streams.outgoing` から除去されるため、タイマーの保持値に (group_id, subgroup_id) を追加し、tick 時に現在の購読者値と override から effective を再計算する。
- `tick_subscription_timeouts` の OBJECT_DELIVERY_TIMEOUT 判定は、stream の (group_id, subgroup_id) に対応する override を反映した effective 値で行う。SUBGROUP_DELIVERY_TIMEOUT 判定は、FIN タイマーが保持する (group_id, subgroup_id) に対応する override を反映した effective 値で行う。
- override は、RESET 終端の場合と、FIN 終端で FIN タイマーを開始しない場合に `close_outgoing_subgroup_stream` で削除する。FIN タイマーを開始する場合は、タイマー発火時に対応する override を削除する。`forget_subscription` では subscription ごと破棄される。
- Properties の decode に失敗した場合は override を登録しない。不正な Properties の拒否は既存のエンコーダ (`SubgroupObject::encode`) の検証に委ね、この issue では properties の妥当性検証を追加しない。
- `send_object_datagram` は §5.2 の override 対象外であり、既存の `effective_object_ms` / `effective_subgroup_ms` と `LocalDatagramTimeout` による drop 判定を変更しない。
- `examples/tokio-moq/src/moqt_client.rs` の `send_subgroup_object` は `SendRequestError::LocalSubgroupTimeout` を `ObjectFilterOutcome::Skip` と同じ扱い (ワイヤ送信しない) にし、呼び出し側は当該 Object を書かずに処理を継続する。`ObjectFilterOutcome::Skip` の doc も delivery timeout を含むように更新する。stream の reset は `SessionEvent::ResetDataStream` を処理する I/O 層が行う。

## 完了条件

- 先頭 Object の Object Property override が、送信側の OBJECT_DELIVERY_TIMEOUT (tick と送信直前チェックの両方) と SUBGROUP_DELIVERY_TIMEOUT (FIN タイマー) に反映されること。override が Track Property より長い場合に早く reset されず、短い場合に遅く reset されないこと。
- 期限切れの subgroup Object を再送しようとすると、`ResetDataStream` (`STREAM_DELIVERY_TIMEOUT`) が発行され、`SendRequestError::LocalSubgroupTimeout` が返ること。Object は送信されず、最大位置も更新されず、stream 追跡は I/O 層の終端通知まで保持されること。
- Track Property のみのケースでも subgroup Object の送信前チェックが働くこと。
- override が、RESET 終端の場合と FIN タイマー不要時の `close_outgoing_subgroup_stream` で削除され、FIN タイマー開始時はタイマー発火時に削除され、`forget_subscription` では subscription ごと破棄されること。
- datagram は §5.2 の override 対象外であり、既存の `LocalDatagramTimeout` の drop 判定が維持されること。
- `examples/tokio-moq/src/moqt_client.rs` の `send_subgroup_object` が `LocalSubgroupTimeout` を `Skip` として扱い、example の publish ループがエラー終了しないこと。
- 送信側 override と送信直前チェックを `tests/test_session/subscription/delivery.rs` に追加して固定すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
