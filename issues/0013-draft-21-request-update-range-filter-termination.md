# REQUEST_UPDATE の Range Filter 拒否で subscription を終端する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-request-update-range-filter-termination
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions) の MUST を満たす。REQUEST_UPDATE が Range Filter 検証で失敗したとき、自側が publisher の subscription を `Terminated` に遷移させ、PUBLISH_DONE(UPDATE_FAILED) を送る。REQUEST_ERROR だけを送って `Established` のまま残る孤立状態をなくす。

## 現状

`src/session/subscription/recv.rs` の `handle_peer_request_update` は、scope 検証の後に `src/session/core.rs` の `check_incoming_range_filters` を呼ぶ。これが失敗すると `emit_request_error` で REQUEST_ERROR (INVALID_FILTER) + FIN を送るだけで、次が行われない。

- subscription は `Established` のままで、自側 publisher の場合に §9.5.1 が MUST とする PUBLISH_DONE(UPDATE_FAILED) が送られない
- `RequestTerminated` 自体は peer が bidi request stream を終端したときに `recv_request_stream_closed` → `close_subscription_on_stream_end` で発行されるため届くが、拒否時点では state が `Terminated` に遷移しない

同じ REQUEST_UPDATE 経路には 2 つ目の検証がある。`handle_update_for_subscription` の累積上限チェック (`merged.count_range_filters() > self.local_max_filter_ranges()`) も `emit_request_error` を呼ぶだけで、同じ孤立状態になる。

`check_incoming_range_filters` は次の 5 経路から呼ばれ、REQUEST_ERROR の送出までを関数内で行う。このため呼び出し元が request 種別に応じた終端処理を選べない。

- SUBSCRIBE (`src/session/subscription/recv.rs` の `handle_peer_subscribe`)
- PUBLISH (`src/session/subscription/recv.rs` の `handle_peer_publish`)
- REQUEST_UPDATE (`src/session/subscription/recv.rs` の `handle_peer_request_update`)
- FETCH (`src/session/fetch.rs` の `handle_peer_fetch`)
- SUBSCRIBE_TRACKS (`src/session/namespace/track_subscription.rs` の `handle_peer_subscribe_tracks`)

既存テスト `tests/test_session/subscription/subscription_limits.rs` の `assert_invalid_filter_rejection` は「REQUEST_ERROR 以外の `SendOnStream` は想定外」としており、`request_update_credit_recovers_after_protocol_level_error` などのテストは拒否後も同じ subscription へ REQUEST_UPDATE を送れる前提を固定している。

対象外:

- FETCH の REQUEST_UPDATE は `FETCH_UPDATE_ALLOWED_PARAMS` (`src/message.rs`) に Range Filter が含まれず、`check_incoming_range_filters` より先の scope 検証で PROTOCOL_VIOLATION のセッションクローズになる (`tests/test_session/subscription/range_filter_scope_order.rs` の
  `fetch_context_range_filter_closes_session`)。Range Filter 検証失敗には到達しないため本 issue の対象外とする
- SUBSCRIBE_TRACKS の §9.5.1 の「bidi stream を閉じる MUST」は、現行の `emit_request_error` の FIN と、peer 終端時の `close_track_subscription_on_stream_end` で成立しているため変更しない
- namespace 系拒否の `emit_request_error` 利用 (`PREFIX_OVERLAP` 等) は本 issue の対象外

根拠 (draft-ietf-moq-transport-21 §9.5.1):

> "When a REQUEST_UPDATE is unsuccessful, the publisher MUST also terminate the subscription by sending a PUBLISH_DONE with error code UPDATE_FAILED."

## 設計方針

- `check_incoming_range_filters` から REQUEST_ERROR の送出を外し、失敗理由を戻り値で呼び出し元へ返す。初回 request の 4 経路 (SUBSCRIBE / PUBLISH / FETCH / SUBSCRIBE_TRACKS) は従来どおり `emit_request_error` で REQUEST_ERROR + FIN を返す (挙動不変)。
- `handle_peer_request_update` の失敗を、request 種別と role で分岐させる。
  - Subscription かつ `my_role == Publisher`: `send_request_error` と同じ終端経路にする。REQUEST_ERROR (INVALID_FILTER) → `Terminated` → PUBLISH_DONE(UPDATE_FAILED)。open 中の outgoing subgroup stream がある場合は §9.9 (PUBLISH_DONE) の MUST NOT に従い `pending_publish_done` へ保留し、全 stream
    終端後に自動送信する。REQUEST_ERROR は FIN せず、PUBLISH_DONE が最終メッセージになる。
  - Subscription かつ `my_role == Subscriber`: PUBLISH_DONE を送るのは publisher である peer の責務であり、`send_err_for_subscription` は Established かつ publisher 以外で `self.fail()` する (セッションクローズになる)。従来どおり `emit_request_error` のみとし、state の終端は peer の PUBLISH_DONE / bidi 終端処理に委ねる。
  - TrackSubscription: 従来どおり `emit_request_error` のみ (対象外の理由は現状を参照)。
- `handle_update_for_subscription` の累積上限超過も同じ role 分岐で終端する。拒否時に `pending_update_params` を更新しない既存契約は維持する。
- `emit_request_error` の利用制限はしない。未登録 request の初回拒否だけでなく、登録済み request への応答 (累積上限超過や `PREFIX_OVERLAP`) にも現に使われており、doc もその前提になっている。
- 既存テストの追従: `subscription_limits.rs` の `assert_invalid_filter_rejection` を新挙動 (REQUEST_ERROR → PUBLISH_DONE) に合わせ、拒否後の同一 request への REQUEST_UPDATE を前提にした `request_update_credit_recovers_after_protocol_level_error` / `repeated_protocol_level_errors_keep_update_limit` /
  `cumulative_overflow_does_not_mutate_pending_params` を `Terminated` 後の期待へ書き換える。MAX_REQUEST_UPDATES のクレジット回復は、拒否後に同一 request の 2 通目が `TOO_MANY_REQUEST_UPDATES` にならないこと (または終端前の別 request) で検証する。

## 完了条件

- 自側 publisher の subscription で単一メッセージの Range Filter 上限超過を注入すると、REQUEST_ERROR (INVALID_FILTER) → `Terminated` → PUBLISH_DONE(UPDATE_FAILED) の順にイベントが出ること
- open 中の outgoing subgroup stream がある場合は PUBLISH_DONE が保留され、全 stream 終端後に送信されること
- 累積上限超過でも同じ終端になること
- 拒否時に `pending_update_params` が更新されないこと
- 自側 subscriber の subscription で peer publisher 発 REQUEST_UPDATE が拒否される場合、PUBLISH_DONE を送らず、セッションも閉じないこと
- SUBSCRIBE / PUBLISH / FETCH / SUBSCRIBE_TRACKS の Range Filter 拒否挙動が従来どおりであること
- `tests/test_session/subscription/subscription_limits.rs` の既存テストが新挙動に追従し、`cargo test --workspace` と PBT が通ること
- 単一メッセージ・累積・role 別の回帰テストが `tests/test_session/subscription/` に追加されていること
