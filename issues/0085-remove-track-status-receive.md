# TRACK_STATUS の受信側 state を削除し send 側だけ残す

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/remove-track-status-receive
- Polished: {YYYY-MM-DD}

## 目的

TRACK_STATUS は subscriber が publisher へ送る要求であり、publisher 側の受信処理は SUBSCRIBE と同じ扱いをする重複実装である。moqt-rs を client / server の endpoint 専用に絞るにあたり、受信側の専用 state を削除して保守対象を減らす。

## 現状

`src/session/track_status.rs` (339 行) と `tests/test_session/namespace/track_status.rs` (789 行) の実装とテストを抱えている。後者は 0084 の実装で削除済みである。

- draft-ietf-moq-transport-21 §9.13 (TRACK_STATUS): "A potential subscriber sends TRACK_STATUS as the first and only message on a new bidi stream to obtain information about the current status of a given track."
- 同節: "The receiver of a TRACK_STATUS message treats it identically as if it had received a SUBSCRIBE message, except it does not create downstream subscription state or send any Objects."

実装箇所は次のとおり。

- `src/session/track_status.rs` の `Session::handle_peer_track_status` / `send_ok_for_track_status` / `send_err_for_track_status` / `close_track_status_on_stream_end` と `track_status_request` / `track_status_requests` / `forget_track_status`
- `src/session/core.rs` の `track_status_requests` テーブルと `RequestTable::TrackStatus` の dispatch
- `src/session/types.rs` の `TrackStatusEntry` / `TrackStatusResponse` (`SessionEvent` に TRACK_STATUS 専用の variant はなく、`RequestOkReceived` / `RequestErrorReceived` を共有している)
- 送信側は `Session::send_track_status` であり、本 issue では削除しない

## 設計方針

- **実装着手前の再確認で、送信側と受信側が単一の `track_status_requests` テーブルと `TrackStatusEntry::my_role` を共有していることが分かった。**「受信側 state テーブルを削除する」と「送信側の応答処理を残す」は両立しないため、削除範囲を次のように限定する。
  - 削除するのは受信側 (自側 publisher) の処理である。`handle_peer_track_status` の受信ハンドラ、`RequestTable::TrackStatus` (peer 起点の逆引き)、`send_ok_for_track_status` / `send_err_for_track_status` / `close_track_status_on_stream_end` の自側 publisher 分岐である。
  - 残すのは送信側 (自側 subscriber) の処理である。`send_track_status` と、REQUEST_OK / REQUEST_ERROR 受信時に `TrackStatusEntry` を完了させる `handle_ok_for_track_status` / `handle_err_for_track_status` である。
  - テーブルは削除せず、`my_role` が常に `TrackStatusEntry` を送信側専用の形に整理する (`my_role` フィールドを削除し、`session::types` の該当型を送信側専用として書き直す)。
- `RequestTable::TrackStatus` の削除可否は、`TrackStatusEntry` の完了処理が request_id から直接引けるため `RequestTable` を経由せずに済むかを実装時に確認して決める。削除できる場合は `RequestTable` をさらに縮小し、削除できない場合は variant を残す。
- `ControlMessage::TrackStatus` と `Session::send_track_status` は残す。client が TRACK_STATUS を送る用途を維持するためである。
- 削除後、peer から TRACK_STATUS を受信した場合は `recv_request` の fallback 経路で `SESSION_PROTOCOL_VIOLATION` としてセッションを閉じる。これは「client / server 専用で TRACK_STATUS の応答を実装しない」という判断の帰結であり、他の未対応 request と同じ扱いにする。
- `tests/test_session/namespace/track_status.rs` は削除済みである。送信側テストは削除対象の受信側テストと同居していたため、送信側で残す価値のあるケース (REQUEST_OK / REQUEST_ERROR の完了処理、`forget_track_status` の冪等性) を `tests/test_session/track_status.rs` として再構成する。
- 本 issue は namespace 発見・告知機構の削除 (0084) の完了後に実施する。

## 完了条件

- `Session` から TRACK_STATUS の受信側 (自側 publisher) の処理が削除されていること
- `Session::send_track_status` と `ControlMessage::TrackStatus` が残り、送信側の応答処理が動作すること
- peer から TRACK_STATUS を受信したときにセッションが `SESSION_PROTOCOL_VIOLATION` で閉じることがテストで固定されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
- `docs/IMPLEMENTATION.md` と `skills/shiguredo-moqt/SKILL.md` の TRACK_STATUS の記述が削除後の実装と一致していること
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリが追加されていること
