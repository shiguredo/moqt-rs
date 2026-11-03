# TRACK_STATUS_OK と FETCH_OK の EXPIRES を扱う

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.3、§9.19.16](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` の `TRACK_STATUS_OK_ALLOWED_PARAMS` は LARGEST_OBJECT だけであり、`src/session/core.rs` が EXPIRES を拒否する。
`src/session/track_status.rs` の応答 state も EXPIRES を保持しない。
FETCH_OK は専用メッセージでパラメータを許していない。

## 設計方針

EXPIRES をすべての OK 応答で許可する。
FETCH_OK の REQUEST_OK 統合と整合させる。
TRACK_STATUS_OK の値は仮に SUBSCRIBE_OK を返すなら提示した値であり、終了済みの TRACK_STATUS 要求を再び生かしたり期限タイマーを起動したりしない。
受信イベントまたは応答参照 API で値を利用できるようにする。

## 完了条件

- TRACK_STATUS_OK / FETCH_OK の EXPIRES が codec と直接 Session API の双方で受理されること。
- TRACK_STATUS_OK の EXPIRES を仮の購読期限として参照でき、単発要求の FIN・破棄条件に影響しないこと。
- 既存の PUBLISH_OK / REQUEST_UPDATE_OK / SUBSCRIBE_OK の許可条件を維持すること。

## 依存・関連

- 関連: [0382](0382-moqt-draft-23-fetch-ok-request-ok.md)

## 解決方法

`src/message.rs` の `TRACK_STATUS_OK_ALLOWED_PARAMS` を `EXPIRES` と `LARGEST_OBJECT` の 2 型に広げ、draft-ietf-moq-transport-23 §9.3 (REQUEST_OK) の "TRACK_STATUS_OK: EXPIRES, LARGEST_OBJECT" に合わせた。
codec 層の `REQUEST_OK_ALLOWED_PARAMS` は既に全応答 context の和集合 (EXPIRES / LARGEST_OBJECT) で検証しており、`FETCH_OK_ALLOWED_PARAMS` / `SUBSCRIBE_OK_ALLOWED_PARAMS` / `PUBLISH_OK_ALLOWED_PARAMS` / `REQUEST_UPDATE_OK_ALLOWED_PARAMS` は変更していない。
`src/session/core.rs` の `handle_peer_request_ok` は `TRACK_STATUS_OK_ALLOWED_PARAMS` を引くだけであるため変更していない。

`src/session/types.rs` の `TrackStatusResponse::Ok` に `expires_ms: Option<u64>` を追加し、`TrackStatusEntry::expires_ms()` で参照できるようにした。
draft-ietf-moq-transport-23 §9.19.16 (EXPIRES Parameter) の "EXPIRES in TRACK_STATUS_OK indicates the value the sender would have included in a SUBSCRIBE_OK for the same Track." により、この値は TRACK_STATUS 要求自体の期限ではなく、同じ Track を SUBSCRIBE した場合に提示されると見込まれる仮の購読期限である。
`src/session/track_status.rs` の `handle_ok_for_track_status` は受信した値だけを保持し、`DeadlineTimer` を作らない。TRACK_STATUS の entry を破棄できるかは `is_track_status_discardable` が `response` の有無だけを見るため、FIN の交換と `forget_track_status` の受理条件は EXPIRES の有無で変わらない。
EXPIRES=0 と不在は `MessageParameters::expires()` の既存の正規化によりどちらも `None` になる。応答を送る側 (`send_ok_for_track_status`) は peer から受信した値ではないため `expires_ms: None` を記録する (LARGEST_OBJECT と同じ扱い)。

`src/session/fetch.rs` の `Fetch` に `expires_ms: Option<u64>` を追加し、初回 FETCH_OK の受信 (`handle_ok_for_fetch_initial`) と `send_fetch_ok` で値を保持する。
FETCH_OK の EXPIRES は sender が request を終了するまでの時間 (§9.19.16) であるが、本実装は FETCH の期限タイマーも起動せず、利用側が期限を判断するための情報として扱う。
`SessionEvent::RequestOkReceived` は `parameters` を運ぶため、TRACK_STATUS_OK の EXPIRES は `parameters.expires()` からも参照できる。
FETCH_OK の EXPIRES は `SessionEvent::FetchOkReceived` の `expires_ms` にも載せ、イベントから直接参照できるようにした。

`docs/moqt.md` と `skills/shiguredo-moqt/SKILL.md` に、すべての OK 応答が EXPIRES を運べること、TRACK_STATUS_OK の値は仮の購読期限であり期限タイマーを起動しないこと、`Fetch::expires_ms` と `TrackStatusEntry::expires_ms` で参照できることを追記した。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| TRACK_STATUS_OK / FETCH_OK の EXPIRES を codec が受理する | `tests/test_message.rs` の `request_ok::allowed_params_are_accepted` が REQUEST_OK の EXPIRES と LARGEST_OBJECT を encode できることを検証する。codec は応答 context を区別しないため、この受理が TRACK_STATUS_OK / FETCH_OK の双方に対応する |
| TRACK_STATUS_OK の EXPIRES を直接 Session API が受理する | `track_status_ok_with_expires_records_tentative_subscription_expiry` が `send_request_ok` の送信と `recv_stream_message` の受信を、PBT の `track_status_ok_expires_is_recorded_and_does_not_affect_discard` が 0..=10,000 ms の任意値での受理を検証する |
| FETCH_OK の EXPIRES を直接 Session API が受理する | `tests/test_session/parameter_rules.rs` の `fetch_ok_with_expires_is_accepted_and_stored` が `send_fetch_ok` の送信と初回 FETCH_OK の受信、`Fetch::expires_ms` と `FetchOkReceived.expires_ms` からの参照を検証する |
| TRACK_STATUS_OK の EXPIRES を仮の購読期限として参照できる | 上記テストが `TrackStatusEntry::expires_ms()` と `RequestOkReceived.parameters.expires()` の双方で `Some(30_000)` を参照できることを、`track_status_ok_with_zero_expires_has_no_expiry` が EXPIRES=0 の `None` 正規化を検証する |
| EXPIRES が単発要求の FIN・破棄条件に影響しない | `track_status_ok_expires_does_not_start_deadline_timer` が EXPIRES (10 ms) を大きく超えて `tick` しても `CloseSession` / `RequestTerminated` を発行せず、FIN の交換後に `forget_track_status` できることを検証する。上記の PBT も同じ性質を任意値で検証する |
| 既存の PUBLISH_OK / REQUEST_UPDATE_OK / SUBSCRIBE_OK の許可条件を維持する | 許可集合を変更していない。`publish_ok_with_largest_object_rejected`、`request_update_ok_subscription_with_expires_accepted`、`request_update_ok_fetch_with_expires_and_largest_object_accepted`、`subscribe_ok_expires_arms_and_expires_on_tick` が挙動を固定する |
| 許可集合を広げすぎていない | `track_status_ok_disallowed_parameter_is_rejected` が AUTHORIZATION_TOKEN を送信側で `SESSION_PROTOCOL_VIOLATION`、受信側で `SESSION_INVALID_PARAMETER` として拒否することを検証する |
| 検証コマンド | `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、`RUSTDOCFLAGS="-D warnings" cargo doc -p shiguredo_moqt --no-deps` がすべて成功した |
