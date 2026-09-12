# SUBSCRIBE_TRACKS の REQUEST_UPDATE で role / state の MUST close を Range Filter 拒否で隠さない

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-track-update-validation-order
- Polished: {YYYY-MM-DD}

## 目的

SUBSCRIBE_TRACKS の REQUEST_UPDATE 受信で、role / state が draft の MUST close に該当する組合せでも Range Filter の拒否が先に成立して REQUEST_ERROR のみを返してしまう問題をなくし、draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE) / §9.20.19 (FORWARD Parameter) の MUST close を確実に発火させる。

## 現状

`src/session/subscription/recv.rs` の `Session::handle_peer_request_update` は、`handle_update_for_track_subscription` へ dispatch する前に `Session::check_incoming_range_filters` を実行し、失敗時は `Session::reject_request_update_range_filters` で終端する。

`reject_request_update_range_filters` は `self.subscriptions` (track 単位 subscription) だけを参照して role / state を分岐する。SUBSCRIBE_TRACKS は `self.track_subscriptions` にあるため、TrackSubscription の request_id では `None` になり `_` 腕に落ちて `emit_request_error(INVALID_FILTER)` を送るだけで `Ok` を返す。その結果、
`src/session/namespace/track_subscription.rs` の `handle_update_for_track_subscription` にある次の MUST close に到達しない。

- role 検証: 自側が SUBSCRIBE_TRACKS の subscriber で peer publisher から REQUEST_UPDATE を受信する組合せ (§9.5: "An endpoint that receives a REQUEST_UPDATE other than in the two cases above MUST close the session with a PROTOCOL_VIOLATION.")
- state 検証: track subscription が Terminated (先行の REQUEST_UPDATE 失敗応答後など) で REQUEST_UPDATE を受信する組合せ

一時テストで次の 2 ケースを確認した (どちらも `recv_stream_message` が `Ok(())` を返し、REQUEST_ERROR + fin のみが発行され、セッションは `Established` のまま)。

- 自側 subscriber の Established で、peer publisher 発の REQUEST_UPDATE に Range Filter 違反 (自側 MAX_FILTER_RANGES 超過) を載せた場合
- 自側 publisher の Terminated で、pipelined に届いた 2 通目の REQUEST_UPDATE に Range Filter 違反を載せた場合

また、`check_incoming_range_filters` が dispatch より前にあるため、Range Filter 違反と FORWARD 値域外 (§9.20.19 の MUST close) が同一メッセージにある場合、FORWARD の close より INVALID_FILTER の REQUEST_ERROR が優先される。FORWARD 値域外はデコード層 (`validate_uint8_param_value`) で拒否されるため、この組合せは Session の API 経由で `MessageParameters` を直接構築した場合に限られる。

さらに、Established publisher の TrackSubscription で Range Filter を拒否する場合、`send_err_for_track_subscription` のような `Terminated` 遷移が行われず、state が `Established` のまま残る (REQUEST_ERROR の fin で bidi stream の送信方向は閉じるが、以後の REQUEST_UPDATE を受理してしまう)。

## 設計方針

- TrackSubscription の REQUEST_UPDATE は、Range Filter の内容検証より前に role / state を判定し、MUST close に該当する組合せでは REQUEST_ERROR を返さず PROTOCOL_VIOLATION でセッションを閉じる。
- Established publisher の TrackSubscription で Range Filter を拒否する場合は `send_err_for_track_subscription` と同じ終端 (Terminated 遷移 + REQUEST_ERROR の fin) にする。
- FORWARD の値域検証も Range Filter の内容検証より前に置き、複合違反では §9.20.19 の close を優先する (Subscription の `handle_update_for_subscription` と検証順を揃える)。
- Subscriptions / namespace 系 (`subscriptions` テーブル) の既存挙動は変えない。

## 完了条件

- 上記 2 ケース (subscriber 側 role 違反 / Terminated 側) で `SESSION_PROTOCOL_VIOLATION` によりセッションが `Closing` になること
- Established publisher の TrackSubscription の Range Filter 拒否で state が `Terminated` になり、REQUEST_ERROR (fin 付き) が送られること
- FORWARD 値域外 + Range Filter 違反の複合時に、FORWARD の MUST close が優先されること
- 正常系 (subscriber 発の valid な REQUEST_UPDATE) の既存テストが変わらず通ること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
