# Mandatory Track Property 受信時の cancel で RESET_STREAM / STOP_SENDING を発行する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-cancel-request-on-mandatory-track-property
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §3.6 (Mandatory Track Properties) は、理解できない Mandatory Track Property を PUBLISH / SUBSCRIBE_OK / FETCH_OK で受けた端末に、その Track を処理も転送もしてはならないと定める。

> "When an endpoint receives a Mandatory Track Property in PUBLISH, SUBSCRIBE_OK, or FETCH_OK that it does not understand, it MUST NOT process or forward that track:"
>
> "*  For SUBSCRIBE_OK messages: the subscriber MUST cancel the subscription (see Section 6.4.2.3).  If the subscriber is a relay with pending downstream subscribers, it MUST send REQUEST_ERROR with error code UNSUPPORTED_EXTENSION to the downstream subscribers."
>
> "*  For FETCH_OK messages: the subscriber MUST cancel the fetch (see
> Section 6.4.2.3).  If the subscriber is a relay and has not yet sent
> a FETCH_OK or REQUEST_ERROR downstream, it MUST send REQUEST_ERROR
> with error code UNSUPPORTED_EXTENSION to the downstream fetch
> requester.  If the relay has already forwarded data on a fetch
> stream, it MUST reset the stream."

§6.4.2.3 (Request Cancellation and Rejection) の cancel は、開いている方向をストリーム終端で打ち切ることまで含む。

> "Implementations cancel a request by abruptly terminating any
> directions of the stream that are still open, using RESET_STREAM for
> a direction they are sending and STOP_SENDING for a direction they
> are receiving.  An endpoint that has already sent a FIN on its
> sending direction and subsequently wishes to cancel sends
> STOP_SENDING on the receiving direction."

subscription の cancel は subscriber が STOP_SENDING を送る (§3.1 (Subscriptions))。

> "The subscriber terminates a subscription in the Pending (Subscriber) or Established states by sending STOP_SENDING."

FETCH では bidi request stream への STOP_SENDING が MUST である (§3.2.1 (Fetch State Management))。

> "If the data stream is already open, the subscriber wishing to cancel the FETCH MAY send STOP_SENDING for the data stream as well as the bidi request stream.  It MUST send STOP_SENDING for the bidi request stream."

現状はローカル状態を終端してアプリへ通知するだけで、cancel の指示が I/O 層へ出ない。peer から見ると購読が生きたままになり、Objects が送られ続ける。

## 現状

- `src/session/subscription/recv.rs` の `Session::handle_peer_subscribe_ok` は `TrackProperties::has_unknown_mandatory` を検出すると
  subscription を `SubscriptionState::Terminated` にし、control message deadline と request stream GOAWAY deadline を解除して
  `SessionEvent::RequestTerminated { kind: RequestKind::Subscribe, reason: TerminationReason::LocalCancel }` を積むだけである
- `src/session/fetch.rs` の `Session::handle_peer_fetch_ok` も同様に fetch を `FetchState::Terminated` にして `SessionEvent::RequestTerminated { kind: RequestKind::Fetch, reason: TerminationReason::LocalCancel }` を積むだけである
- どちらも `SessionEvent::StopSendingRequestStream` / `SessionEvent::ResetRequestStream` を発行しない。`src/session/types.rs` の `StopSendingRequestStream` doc も「Session は Malformed Track 検出時 (§12.1) に、新規に `Terminated` へ遷移させる経路で本イベントを発行する」としており、§3.6 の cancel 経路は対象になっていない
- 同じ cancel の MUST を扱う `src/session/data.rs` の `Session::terminate_malformed_track` は `StopSendingRequestStream` → `ResetRequestStream` の順に `STREAM_MALFORMED_TRACK` で発行しており、§3.6 の経路と非対称である
- `tests/test_session/subscription/handshake.rs` の `subscribe_ok_with_unknown_mandatory_property_cancels_subscription` と `tests/test_session/fetch/validation.rs` の `fetch_ok_with_unknown_mandatory_property_cancels_fetch` が、現在の「Terminated と `RequestTerminated(LocalCancel)` のみ」を固定している

## 設計方針

- `Session::handle_peer_subscribe_ok` と `Session::handle_peer_fetch_ok` の unknown mandatory 検出経路で、`Session::terminate_malformed_track` と同じ順序の cancel イベントを発行する
  - `SessionEvent::StopSendingRequestStream { request_id, error_code: STREAM_CANCELLED }` (受信方向)
  - `SessionEvent::ResetRequestStream { request_id, error_code: STREAM_CANCELLED }` (送信方向)
- エラーコードは §12.5 (Stream Reset Error Codes) の `CANCELLED` (0x1) を使う。§3.6 の cancel は自端の購読キャンセルであり、relay publisher の検出を表す `MALFORMED_TRACK` (0x12) ではない
- `TerminationReason::LocalCancel` と `Terminated` への遷移は維持する (アプリへの通知内容は変えない)
- FETCH では §3.2.1 の MUST である bidi request stream の STOP_SENDING を必ず含める。既に開いている FETCH data stream への STOP_SENDING は MAY であり、本 issue の必須範囲には含めない
- 送信方向が既に FIN 済みの場合に `ResetRequestStream` を I/O 層が無視する既存契約は変えない。その場合も STOP_SENDING は必要である
- cancel イベントは unknown mandatory を検出した経路で 1 回だけ発行する
- `SessionEvent::StopSendingRequestStream` の doc に §3.6 の cancel 経路でも発行されることを追記する
- PUBLISH の受信経路 (`src/session/subscription/recv.rs` の `Session::handle_peer_publish`) は既に `REQUEST_ERROR(UNSUPPORTED_EXTENSION)` で拒否しており、本 issue の対象外とする

## 完了条件

- 未知の Mandatory Track Property を含む SUBSCRIBE_OK を受けたとき、`SessionEvent::StopSendingRequestStream` と `SessionEvent::ResetRequestStream` が `STREAM_CANCELLED` (0x1) で発行されることを固定するテストが `tests/test_session/` に追加されていること
- 未知の Mandatory Track Property を含む FETCH_OK を受けたときも同じ 2 イベントが発行されることを固定するテストが追加されていること
- どちらの経路でも subscription / fetch が `Terminated` になり、`SessionEvent::RequestTerminated { reason: TerminationReason::LocalCancel }` が発行されることが固定されていること
- どちらの経路でもセッションが `SessionState::Closing` / `SessionState::Closed` にならないこと
- 既存の `subscribe_ok_with_unknown_mandatory_property_cancels_subscription` と `fetch_ok_with_unknown_mandatory_property_cancels_fetch` が cancel イベントの検証を含む形に更新されていること
- `cargo test --workspace` が通ること
