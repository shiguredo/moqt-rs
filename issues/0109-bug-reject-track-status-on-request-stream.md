# TRACK_STATUS を受信するとセッションを閉じる

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-reject-track-status-on-request-stream
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.3 (Session initialization) は request stream の先頭として 7 種のメッセージを挙げ、それ以外の型で始まった場合にのみセッションを閉じると規定する。

> "A request stream begins with one of these seven message types: TRACK_STATUS, SUBSCRIBE, PUBLISH, FETCH, PUBLISH_NAMESPACE, SUBSCRIBE_NAMESPACE, and SUBSCRIBE_TRACKS.
> Bidirectional streams MUST NOT begin with any other message type unless negotiated.
> If they do, the peer MUST close the Session with a PROTOCOL_VIOLATION."

TRACK_STATUS は許可された 7 種の 1 つであり、これを受信して `PROTOCOL_VIOLATION` でセッションを閉じるのは §6.3 に反する。§9.13 (TRACK_STATUS) は受信側の扱いを定める。

> "The receiver of a TRACK_STATUS message treats it identically as if it had received a SUBSCRIBE message, except it does not create downstream subscription state or send any Objects.
> If successful, the publisher responds with a TRACK_STATUS_OK with the same parameters and Track Properties it would have set in a SUBSCRIBE_OK.
> Track Alias is not used.
> A publisher responds to a failed TRACK_STATUS with an appropriate REQUEST_ERROR message.
> The bidi stream is closed with a FIN after TRACK_STATUS_OK or REQUEST_ERROR are sent."

現状は正当な TRACK_STATUS の受信でセッション全体が落ちる。応答できない Track であっても、§9.13 のとおり購読単位の REQUEST_ERROR で拒否できる。

## 現状

- `src/session/core.rs` の `Session::recv_request` は `ControlMessage::Subscribe` / `Publish` / `Fetch` のみを dispatch し、`_ =>` で `SESSION_PROTOCOL_VIOLATION` を返して `Session::fail` を呼ぶ
- `src/session/track_status.rs` は送信側のみを実装する (`Session::send_track_status` / `Session::handle_ok_for_track_status` / `Session::handle_err_for_track_status` / `Session::close_track_status_on_stream_end`)。モジュール doc にも publisher 側の受信処理を実装しないと書かれている
- `docs/IMPLEMENTATION.md` は TRACK_STATUS を「subscriber 側の送信と応答受信のみ。publisher 側の受信処理は未実装」と記載する
- 応答送信の土台は残っている。`src/session/core.rs` の `Session::send_request_ok` は `RequestTable::TrackStatus` 用の検証分岐 (`TRACK_STATUS_OK_ALLOWED_PARAMS` の検証と Track Properties の許可) を持つが、
  応答を組み立てる `Session::send_ok_for_track_status` は 0085 で削除済みで、dispatch は `unreachable!` になる。
  `Session::send_request_error` は `RequestTable::TrackStatus` を request_id 未発見として拒否する
- `src/message.rs` の `TRACK_STATUS_ALLOWED_PARAMS` は `AUTHORIZATION_TOKEN` と `INCLUDE_PROPERTIES` のみで、受信側のパラメータ検証にも使える
- `tests/test_session/track_status.rs` の `recv_peer_track_status_closes_session` が現在のセッション終了挙動を固定している
- 0085 で受信側 state を削除した経緯がある。0085 は「client / server 専用で TRACK_STATUS の応答を実装しない」という判断で受信側を削除し、peer から TRACK_STATUS を受信したら `SESSION_PROTOCOL_VIOLATION` で閉じると明記した。`CHANGES.md` の `## develop` にも `[CHANGE]` として記録されている。本 issue はこの判断を §6.3 に合わせて見直す

## 設計方針

- `Session::recv_request` に `ControlMessage::TrackStatus` の dispatch を追加し、SUBSCRIBE と同じ検証経路で受理する。§9.13 のとおり Track Alias を使わず、downstream の subscription state を作らず、Objects も送らない
- 検証は SUBSCRIBE の受信処理 (`src/session/subscription/recv.rs` の `Session::handle_peer_subscribe`) と共通化する。`Session::accept_peer_request` による Request ID の検証、予約名前空間 (§2.4.2) と `.session` 名前空間 (§6.5) の拒否、`Session::emit_request_error` による REQUEST_ERROR 応答を同じ順序で適用する
- 成功応答は `Session::send_request_ok` の `RequestTable::TrackStatus` 経路で送る。§9.3 (REQUEST_OK) のとおり TRACK_STATUS_OK は REQUEST_OK の shorthand であり、Track Properties を載せるのは TRACK_STATUS_OK だけである
- 応答 (TRACK_STATUS_OK / REQUEST_ERROR) の送信後に bidi stream を FIN する (§9.13)。`SessionEvent::SendOnStream` の `fin` を TRACK_STATUS context で true にする。`Session::emit_request_error` は既に `fin: true` で送るため、成功応答側を揃える
- `INCLUDE_PROPERTIES` を検証し (§9.20.22: 0 と 1 以外は `PROTOCOL_VIOLATION` でセッションを閉じる MUST)、0 のときは TRACK_STATUS_OK の Track Properties を空にする (§9.20.22: "If INCLUDE_PROPERTIES is 0, the Track Properties are still present in the message, but they SHOULD be empty.")。SUBSCRIBE の受信処理と同じ検証を使う
- `TrackStatusEntry` は送信側専用の型である (0085 で `my_role` を削除した)。受信側の応答状態を同じテーブルに持つか別に持つかは実装時に決める
- requester の FIN で responder が応答を送れなくなる問題は 0108 で扱う。本 issue の受信実装は 0108 の設計に従う
- `Session::send_track_status` と `ControlMessage::TrackStatus` の送信側は変更しない
- `docs/IMPLEMENTATION.md` と `skills/shiguredo-moqt/SKILL.md` の TRACK_STATUS の記述を更新する
- 現在の挙動を固定する `tests/test_session/track_status.rs` の `recv_peer_track_status_closes_session` は本 issue で置き換える

## 完了条件

- TRACK_STATUS を受信して TRACK_STATUS_OK を返す経路のテストが `tests/test_session/` に追加され、セッションが `SessionState::Closing` / `SessionState::Closed` にならないこと
- 存在しない Track への TRACK_STATUS で REQUEST_ERROR を返すテストが追加されていること
- TRACK_STATUS_OK / REQUEST_ERROR の送信後に bidi stream が FIN されること (`SessionEvent::SendOnStream` の `fin` が true であること)
- `INCLUDE_PROPERTIES=0` の TRACK_STATUS への TRACK_STATUS_OK で Track Properties が空になること
- 受信した TRACK_STATUS が subscription state を作らないこと (`Session::subscription` が `None` のままであること) がテストで固定されていること
- 既存の `Session::send_track_status` と REQUEST_OK / REQUEST_ERROR 受信の挙動 (`tests/test_session/track_status.rs`) が変わらないこと
- `cargo test --workspace` が通ること
