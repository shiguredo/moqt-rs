# GOAWAY の request stream deadline 満了後に遅延状態が残る

- Created: 2026-09-26
- Completed: 2026-10-06
- Branch: feature/fix-goaway-deadline-deferred-state
- Polished: 2026-09-27

## 目的

[issues/closed/0141](../issues/closed/0141-bug-publish-sender-peer-fin-termination.md) で、PUBLISH の送信側は peer FIN を受信しても PUBLISH_DONE を送るまで終端を遅延するようになった。
request stream 上で GOAWAY を送信した側は reset deadline を持ち、deadline が満了して request stream の送信方向を `STREAM_GOING_AWAY` で reset した場合に
遅延状態 (`peer_fin_received` と Established のままの subscription、保留中の `pending_publish_done`) が
整理されないため、整理する。

## 現状

- request stream 上で GOAWAY を送信した側 (`Session::send_goaway_on_request_stream`、`src/session/goaway.rs`) が reset deadline を設定し、満了時に `Session::tick` が `SessionEvent::ResetRequestStream { error_code: STREAM_GOING_AWAY }` を 1 回だけ発行する。受信側 (`Session::handle_peer_goaway_on_request_stream`) は deadline を設定しない
  - draft-ietf-moq-transport-21 §9.2 (GOAWAY): "When sent on a request stream, the sender SHOULD reset the stream with GOING_AWAY after the indicated timeout."
- 0141 の変更で、PUBLISH を送った側 (publisher 役) は peer FIN を受信しても終端せず、`peer_fin_received` に記録して `Established` (保留 PUBLISH_DONE がある場合は `Terminated`) を維持する (`Session::defers_peer_fin`)。同じ遅延は SUBSCRIBE / FETCH / TRACK_STATUS の responder にも適用される
- `Session::tick` の deadline 満了処理は、`pending_publish_done` の破棄と `ResetRequestStream` の発行だけを行い、subscription の state / `peer_fin_received` / `local_fin_sent` / `request_streams` / `RequestTerminated` には手を付けない
  - peer FIN 受信済み (遅延中) の subscription は `Established` のまま残り、`cleanup_ready()` が false のため `Session::forget_subscription` もできず、deadline 満了処理からは `RequestTerminated` が発行されない
  - peer FIN 未受信の `Established` でも、reset 後に届く peer FIN は `Session::defers_peer_fin` 経路に入り `local_fin_sent` が立たないため終端が確定しない (peer の RESET_STREAM は従来どおり終端する)
  - この遅延状態は `Session::goaway_drain_snapshot` の blocker に残り続ける

## 設計方針

- deadline 満了で request stream の送信方向を reset した時点を「PUBLISH_DONE を送れない終端」とみなし、まだ終端していない request (subscription に限らず fetch / track_status も同じ遅延経路を持つ) を終端して `RequestTerminated` を 1 回だけ発行する
  - 終端方法は `Session::terminate_malformed_track` (`src/session/data.rs`) と同じ枠組みにする。state を `Terminated` にし、`request_streams` から除去して `rejected_request_ids` へ移す。以後 peer から届く FIN / RESET_STREAM の close 通知は no-op として吸収され、`RequestTerminated` は二重発行されず、未知 id の `SESSION_PROTOCOL_VIOLATION` にもならない。peer FIN 受信済みの場合は受信方向も閉じており、索引を除去するだけでよい
  - 保留中の `pending_publish_done` は破棄する。bidi request stream の送信方向が reset された後は PUBLISH_DONE を書き込めないためである (`src/session/goaway.rs` の既存コメントと同じ根拠)。§9.9 (PUBLISH_DONE) の MUST NOT は「すべての stream を閉じるまで送ってはならない」という向きの制約であり、破棄の根拠にはできない
  - 既に `Terminated` の request (REQUEST_UPDATE 失敗応答で保留 PUBLISH_DONE を持つ場合など) は `RequestTerminated` を発行せず、保留 PUBLISH_DONE の破棄だけを行う (二重発行の抑止)
- 終端理由は GOAWAY 由来であることが分かるものにする。`TerminationReason` (`src/session/types.rs`) に新 variant `GoawayTimeout` を追加する。公開 enum への variant 追加は破壊的変更のため、`CHANGES.md` の `## develop` に [CHANGE] として記載する
- `peer_fin_received` / `local_fin_sent` / `pending_publish_done` の後始末は `Session::forget_subscription` との重複を作らず、deadline 満了の終端処理に集約する
- 状態の整理漏れを PBT または例示テストで固定する
- 挙動の変更に合わせて `skills/shiguredo-moqt/SKILL.md` の `RequestTerminated` / `ResetRequestStream` の記述を更新する (0141 / 0027 と同じ扱い)

## 完了条件

- GOAWAY の deadline 満了で request stream の送信方向を reset したときに、まだ終端していない PUBLISH 送信側 (publisher 役) の subscription が終端し、`RequestTerminated` が 1 回だけ発行されることを固定するテストが追加されていること
  - peer FIN 受信済み (遅延中) と未受信の両方
- 保留中の PUBLISH_DONE が破棄され、その後に送信されないことを固定するテストが追加されていること (既存の `tests/test_session/goaway.rs` の `request_stream_goaway_reset_discards_pending_publish_done` を維持・拡張する)
- deadline 満了後に peer の FIN / RESET_STREAM が届いても、`RequestTerminated` が二重に発行されず、セッションが `SESSION_PROTOCOL_VIOLATION` にならないことを固定するテストが追加されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

`Session::tick` の deadline 満了処理を、request stream の送信方向を reset するだけでなく、まだ終端していない request を終端するように変更した。

- `src/session/goaway.rs`: `Session::terminate_request_on_goaway_timeout` を追加し、deadline 満了時に subscription /
  fetch / track_status を `Terminated` にして `request_streams` から `rejected_request_ids` へ移し、
  `RequestTerminated { reason: GoawayTimeout }` を 1 回だけ発行する。保留中の `pending_publish_done` は破棄し、
  open 中の fill fetch stream は §3.4.1 の MUST に従い reset する。`Session::goaway_drain_snapshot` は
  応答を送れなくなった (`terminated` の) TRACK_STATUS を blocker にしない
- `src/session/types.rs`: `TerminationReason` に `GoawayTimeout` を追加し (`Copy` も追加)、
  `RequestTerminated` / `ResetRequestStream` / `GoawayDrainSnapshot` / `TrackStatusEntry::terminated` /
  `SubscriptionState::Terminated` / `FetchState::Terminated` / `pending_publish_done` の doc を更新した
- `src/session/core.rs` / `src/session/data.rs` / `src/session/subscription/` / `src/session/fetch.rs` /
  `src/session/track_status.rs`: ローカル終端した request へ遅延して届く peer メッセージを、各ハンドラの
  該当する MUST 検証 (送信者・Track Alias・パラメータスコープ・FORWARD・Track Properties・Redirect・
  End Location) を通したうえで状態遷移せず受理するようにした。遅延した REQUEST_ERROR は
  `RequestErrorReceived` のみ通知する。遅延 data stream は FETCH 応答を破棄対象 stream として吸収し、
  fill fetch stream は subscription への帰属を保ったまま受理する。state テーブルから破棄済み
  (`forget_*` 後) の request へのメッセージは request 種別と状態が失われているため従来どおり違反とする
- `CHANGES.md` の `## develop` に `[CHANGE]` エントリを追加し、`skills/shiguredo-moqt/SKILL.md` を追随させた
- 追加・更新したテスト
  - `tests/test_session/goaway.rs` と `tests/test_session/goaway/deadline.rs`: deadline 満了での終端
    (PUBLISH 送信側の遅延中 / 未受信、fetch、TRACK_STATUS)、遅延応答と遅延 data stream の吸収、
    遅延した REQUEST_UPDATE / PUBLISH_STATE_NOTIFY の受理と違反の検出。テスト量が増えたため
    `fetch` と同じ `#[path]` 方式で `goaway/deadline.rs` に分割した
  - `tests/test_session/fetch/fill.rs` と `tests/test_session/fetch/unified.rs`: 遅延 fill fetch stream と
    FETCH 応答 data stream の受理
  - `pbt/tests/prop_session/goaway.rs`: 任意の tick 時刻で reset と終端が 1 回だけ発行されること
