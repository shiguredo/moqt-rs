# 購読 cancel 後に届く SUBSCRIBE_OK / REQUEST_ERROR でセッションを閉じない

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscription-cancel-late-response
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §3.1 (Subscriptions) は "The subscriber terminates a subscription in the Pending (Subscriber) or Established states by sending STOP_SENDING." と定める。
購読者が Pending(Subscriber) で cancel した直後に、peer が STOP_SENDING を観測する前に送った SUBSCRIBE_OK / REQUEST_ERROR が届くのは正当な競合である。現状はこれを不正状態として `PROTOCOL_VIOLATION` でセッション全体を閉じる誤動作になっている。

本 issue は `stop_sending` と `supersede_pending_subscriber` による cancel を主対象とする。
`is_cancelled_terminated` を使うため、REQUEST_ERROR 受信・peer FIN・GOAWAY timeout などで `Terminated` かつ `publish_done` 未設定になった subscription も同じ吸収経路に入る (トレードオフは設計方針に記載)。`forget_subscription` 後に届く応答は request を特定できないため対象外であり、既存どおり `PROTOCOL_VIOLATION` とする。

## 現状

- `src/session/subscription/recv.rs` の `stop_sending` は subscription を `Terminated` にするだけで、`rejected_request_ids` に記録しないため、既存の `is_locally_terminated_request` 経路では吸収されない。control message 応答待ち deadline も解除しない。timeout を設定している場合、cancel 後に応答が届かなくても deadline 満了で `CONTROL_MESSAGE_TIMEOUT` によりセッションが閉じる。
- `src/session/subscription.rs` の `supersede_pending_subscriber` は `Terminated` にして control message deadline を解除するが、`rejected_request_ids` には記録しないため、同じく `is_locally_terminated_request` 経路では吸収されない。
- `src/session/subscription/recv.rs` の `handle_peer_subscribe_ok` は「Pending(Subscriber) でも拒否済み (`is_locally_terminated_request`) でもない」SUBSCRIBE_OK を `PROTOCOL_VIOLATION` で閉じる。
- `src/session/subscription/dispatch.rs` の `handle_err_for_subscription` も、`rejected_request_ids` に無い `Terminated` subscription への REQUEST_ERROR を同様に閉じる。
- FETCH 側は `src/session/fetch.rs` の `handle_peer_fetch_ok` が `Terminated` を許容してセッションを閉じず、`FetchOkReceived` を発行する。`handle_err_for_fetch` も `Terminated` の REQUEST_ERROR を受理する。subscription だけが `PROTOCOL_VIOLATION` で閉じる点が非対称である。
- 遅延した制御応答を注入するテストは無い (`tests/test_session/request_stream.rs` の supersede テストは旧 request id の forget までしか見ていない)。
- 影響: cancel と応答の競合で全購読が落ちる。`stop_sending` は control message deadline を解除しないため、timeout 設定時は deadline 満了で `CONTROL_MESSAGE_TIMEOUT` によりセッションが閉じる。example の STOP_SENDING 経路でも起こり得る。

## 設計方針

- `Terminated` かつ `publish_done` 未設定 (`is_cancelled_terminated`) の subscription への SUBSCRIBE_OK / REQUEST_ERROR は、状態遷移も `RequestTerminated` の発行もせずに吸収する。
- 判定は `src/session/data.rs` の `is_cancelled_terminated_subscription` を `pub(super)` にして `handle_peer_subscribe_ok` / `handle_err_for_subscription` から共用する。
- `rejected_request_ids` への記録は行わない。同集合は `request_streams` 未登録の拒否 request を対象とする既存契約 (`src/session/core.rs` の `rejected_request_ids` doc) であり、state を持つ cancel 済み subscription を登録すると close 通知時の削除契約と齟齬が出るためである。
- `handle_peer_subscribe_ok` の状態検証と早期 return の条件に `is_cancelled_terminated_subscription` を加える。Track Alias 衝突検証は早期 return より前にあるため残る。
- `handle_err_for_subscription` は `is_locally_terminated_request` に加えて `is_cancelled_terminated_subscription` を早期 return の条件にする。`handle_peer_request_error` の Redirect 検証は `handle_err_for_subscription` より前にあるため残る。
- `handle_peer_request_error` は `handle_err_for_subscription` が `Ok` を返した場合に `RequestErrorReceived` を発行する既存契約を維持する。遅延 REQUEST_ERROR は状態遷移と `RequestTerminated` を発行しないが、`RequestErrorReceived` は通知する。遅延 SUBSCRIBE_OK の吸収はイベントを発行しない。
- `stop_sending` で `Session::clear_control_message_deadline` を呼び、応答待ち deadline を解除する (`supersede_pending_subscriber` は既に解除している)。解除しないと、遅延応答を吸収しても `Session::tick` の `CONTROL_MESSAGE_TIMEOUT` でセッションが閉じる。
- `is_cancelled_terminated` は、REQUEST_ERROR 受信・peer FIN・GOAWAY timeout などの経路で `Terminated` かつ `publish_done` 未設定になった場合にも true になる。いずれも以後の応答を待たない状態であり、遅延応答を吸収する対象として扱う。二重応答の検出は draft §3.1 の SHOULD close であり、終端済みの購読を優先して受理する。
- cancel と応答の到着順を入れ替えたテストを追加する。遅延応答は `forget_subscription` の前、subscription が `Terminated` で残っている間に注入する。

## 完了条件

- Pending(Subscriber) で `stop_sending` した直後に SUBSCRIBE_OK / REQUEST_ERROR を `recv_stream_message` へ渡しても、セッションが閉じず購読だけが終端していること。
- `supersede_pending_subscriber` 経由でも同じであること。
- Established で `stop_sending` した後に届く SUBSCRIBE_OK / REQUEST_ERROR も、同じ経路で状態遷移せず吸収されること。
- `send_subscribe` の前に `tick(0)` と control message timeout を設定し、`stop_sending` した後に `tick` しても、deadline が解除されて `CONTROL_MESSAGE_TIMEOUT` で閉じないこと。
- 遅延 REQUEST_ERROR では `RequestErrorReceived` が 1 回発行され、`RequestTerminated` は追加発行されないこと。
- 遅延 SUBSCRIBE_OK で Track Alias が異なる Track の Established subscription と衝突する場合は、従来どおり `DUPLICATE_TRACK_ALIAS` で閉じること。
- 遅延 REQUEST_ERROR が非ゼロ Connect URI の Redirect を伴う場合は、server 側で従来どおり `PROTOCOL_VIOLATION` により閉じること。
- 上記を `tests/test_session` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
