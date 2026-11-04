# request stream の GOAWAY を要求受信者だけに許可する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.2](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/session/goaway.rs` の `send_goaway_on_request_stream` は要求の起点を検証せずイベントを送出する。
`handle_peer_goaway_on_request_stream` も重複と URI だけを確認するため、request sender から receiver へ送られた GOAWAY を受理する。

## 設計方針

要求の initiator を SUBSCRIBE / PUBLISH / FETCH / TRACK_STATUS ごとに確認し、receiver だけが request stream 上で GOAWAY を送れるようにする。
違反する受信は明示された PROTOCOL_VIOLATION で終了し、送信 API はイベントや期限 state を作る前に拒否する。
client / server role や publisher / subscriber role を request initiator と混同しない。

## 完了条件

- SUBSCRIBE と PUBLISH で sender / receiver が逆になるケース、FETCH / TRACK_STATUS を検証すること。
- 不正な GOAWAY を受理せず、正当な receiver の GOAWAY と session 全体の GOAWAY は従来どおり動くこと。
- 0375 の移行処理とは独立した protocol role 検証として実装すること。

## 依存・関連

- 関連: [0375](0375-draft-23-request-stream-goaway-migration.md)

## 解決方法

request stream 上の GOAWAY の送信者を protocol role で制限した。

- `src/session/core.rs` に `Session::is_request_receiver` を追加した。request の initiator は SUBSCRIBE / FETCH / TRACK_STATUS が subscriber (§9.6 / §9.11 / §9.12)、PUBLISH が publisher (§9.8) である。
- receiver の判定には `Subscription::initiator` と `my_role`、`Fetch::my_role`、`TrackStatusEntry::my_role` を使う。client / server の `Role` とも publisher / subscriber の `TrackRole` 単独とも一致しないため、request 種別ごとの組で判定している。
- `Session::send_goaway_on_request_stream` は event と deadline を積む前に receiver 判定を行い、sender 側からの送信と、自側 state に無い request への送信を `PROTOCOL_VIOLATION` で拒否する。拒否時に session の状態は変更しない。
- `Session::handle_peer_goaway_on_request_stream` は URI 検証の後に receiver 判定を追加した。自側が sender (peer が receiver) の場合と request を特定できない場合は受理し、自側が receiver の場合は §9.2 の MUST に従って `PROTOCOL_VIOLATION` でセッションを閉じる。request を特定できない状態は同節の MUST の条件 (receiver が受け取った場合) が成立しないため、判定不能を理由に閉じない。
- 移行処理には触れておらず、protocol role 検証のみを追加している。

テストは `tests/test_session/goaway/role.rs` を追加し、4 種類それぞれの送信拒否・受信時クローズ・正当な receiver からの GOAWAY の受理を検証した。
既存テストのうち request の sender 側から GOAWAY を送っていたものは、receiver 側から送る形へ書き換えた (PUBLISH 起点は subscriber、SUBSCRIBE / FETCH / TRACK_STATUS 起点は publisher)。
sender 側からの GOAWAY を前提にしていた遅延応答の吸収テストは、新仕様では発生しない組み合わせのため削除し、同じ吸収経路を malformed track 終端からの遅延 SUBSCRIBE_OK / FETCH_OK と、PUBLISH_DONE / PUBLISH_STATE_NOTIFY の遅延受信で検証する形に置き換えた。
ただし遅延 FETCH data stream の吸収だけは削除したテストでしか通っていなかったため、malformed track 終端で同じ Track の FETCH を終端した後に
遅延 FETCH data stream が届く経路 (`Session::tolerates_late_data_stream` の `rejected_request_ids` 判定) を
`tests/test_session/data_stream.rs` の `malformed_track_absorbs_late_fetch_data_stream_on_terminated_fetch` で検証する。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| SUBSCRIBE と PUBLISH で sender / receiver が逆になるケース | `tests/test_session/goaway/role.rs` の `subscribe_*` と `publish_*` の送信拒否・受信クローズ 4 件 |
| FETCH / TRACK_STATUS の検証 | 同ファイルの `fetch_*` と `track_status_*` の送信拒否・受信クローズ・受理 6 件 |
| 不正な GOAWAY を受理しない | 上記の受信時クローズと `unknown_request_recv_goaway_closes_session` で `PROTOCOL_VIOLATION` と `Closing` を検証 |
| 正当な receiver の GOAWAY が従来どおり動く | 同ファイルの受理テスト、`tests/test_session/goaway.rs` の `goaway_on_request_stream_server_to_client` / `goaway_on_request_stream_client_to_server_zero_uri`、`tests/test_session/goaway/deadline.rs` の receiver 側 deadline テスト |
| session 全体の GOAWAY が従来どおり動く | `tests/test_session/goaway.rs` の `goaway_on_control_stream_has_none_on_request_stream` / `peer_goaway_returns_the_received_info` / `goaway_drain_ready_is_emitted_once` など |
| 移行処理とは独立した protocol role 検証 | `src/session/goaway.rs` と `src/session/core.rs` だけで判定し、`crates/` の移行処理に変更を加えていない |
| 検証コマンド | `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` がすべて成功 |
