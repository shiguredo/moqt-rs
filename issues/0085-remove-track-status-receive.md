# TRACK_STATUS の受信側 state を削除し send 側だけ残す

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-track-status-receive
- Polished: {YYYY-MM-DD}

## 目的

TRACK_STATUS は subscriber が publisher へ送る要求であり、publisher 側の受信処理は SUBSCRIBE と同じ扱いをする重複実装である。moqt-rs を client / server の endpoint 専用に絞るにあたり、受信側の専用 state を削除して保守対象を減らす。

## 現状

`tests/test_session/namespace/track_status.rs` (789 行) を含む実装とテストを抱えている。

- draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): "A potential subscriber sends TRACK_STATUS as the first and only message on a new bidi stream to obtain information about the current status of a given track."
- 同節: "The receiver of a TRACK_STATUS message treats it identically as if it had received a SUBSCRIBE message, except it does not create downstream subscription state or send any Objects."

実装箇所は次のとおり。

- `src/session/namespace/track_status.rs` の `Session::handle_peer_track_status` / `send_ok_for_track_status` / `send_error_for_track_status` と `track_status_request` / `track_status_requests` / `forget_track_status`
- `src/session/core.rs` の `track_status_requests` テーブルと `RequestTable::TrackStatus` の dispatch
- `src/session/types.rs` の `TrackStatusEntry` / `TrackStatusResponse` (`SessionEvent` に TRACK_STATUS 専用の variant はなく、`RequestOkReceived` / `RequestErrorReceived` を共有している)
- 送信側は `Session::send_track_status` であり、本 issue では削除しない

## 設計方針

- 受信側の state テーブルと handler、`RequestTable::TrackStatus`、`TrackStatusEntry` / `TrackStatusResponse`、`SessionEvent::TrackStatusReceived` を削除する。
- `ControlMessage::TrackStatus` と `Session::send_track_status` は残す。client が TRACK_STATUS を送る用途を維持するためである。
- 削除後、peer から TRACK_STATUS を受信した場合は `recv_request` の fallback 経路で `SESSION_PROTOCOL_VIOLATION` としてセッションを閉じる。これは「client / server 専用で TRACK_STATUS の応答を実装しない」という判断の帰結であり、他の未対応 request と同じ扱いにする。
- `tests/test_session/namespace/track_status.rs` は削除する。`send_track_status` (送信側) のテストが同ファイルにある場合は、送信側テストを残すために `tests/test_session/track_status.rs` などへ移す。
- 本 issue は namespace 発見・告知機構の削除と作業範囲が重なるため、そちらの完了後に実施する。

## 完了条件

- `Session` から TRACK_STATUS の受信側 state と handler、公開 API が削除されていること
- `Session::send_track_status` と `ControlMessage::TrackStatus` が残っていること
- peer から TRACK_STATUS を受信したときにセッションが `SESSION_PROTOCOL_VIOLATION` で閉じることがテストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
- `docs/IMPLEMENTATION.md` と `skills/shiguredo-moqt/SKILL.md` の TRACK_STATUS の記述が削除後の実装と一致していること
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリが追加されていること
