# PUBLISH 起点 publisher の保留 PUBLISH_DONE が request stream の FIN で失われる

- Created: 2026-10-09
- Completed: 2026-10-13
- Branch: feature/fix-moqt-client-publish-done-after-finish-request-stream
- Polished: {YYYY-MM-DD}
- Updated: 2026-10-11

## 目的

publisher 役で REQUEST_UPDATE を拒否したときの PUBLISH_DONE (`UPDATE_FAILED`) の送信は draft-ietf-moq-transport-22
§9.5.1 (Updating Subscriptions) が MUST で求める。同 draft §9.9 (PUBLISH_DONE) は「全ての stream を閉じるまで
送らない」MUST NOT も定めるため、open 中の outgoing data stream がある間は送信を保留し、無くなった時点で送ることになる。
この保留中に自側が request stream の送信方向を FIN すると、その後の flush が `SendOnStream` を発行しても
送信半が台帳に無く「Dropping send on already closed request stream」で捨てられ、§9.5.1 の MUST を果たせない。
ただし現行の Session は PUBLISH 起点 publisher の peer FIN を `Session::defers_peer_fin` で遅延するため
`SessionEvent::FinishRequestStream` は発行されず、この FIN 自体が起きない (詳細は現状を参照)。

## 現状

- `src/session/core.rs` の `Session::recv_request_stream_closed(RequestStreamEnd::Fin)` は、自側が
  SUBSCRIBE / FETCH / TRACK_STATUS の requester のとき `SessionEvent::FinishRequestStream` を発行する。
  PUBLISH 起点 publisher の request では `Session::defers_peer_fin` が先に評価されて peer FIN を
  遅延するため、このイベントは発行されない (0141 で実装済み)。
- `examples/tokio-moq/src/moqt_client.rs` の `drain_events` の `SessionEvent::FinishRequestStream` 分岐は
  `bidi_sends` から送信半を取り出して `finish()` する。`Subscription::pending_publish_done` を見ていない。
- `Session::maybe_flush_pending_publish_done` は open 中の outgoing data stream が無くなると
  `SendOnStream { fin: true }` を積むが、送信半が既に無い場合は example が warn を出して捨てる
  (同じ `drain_events` の `SendOnStream` 分岐の else 経路)。
- 0201 で回収の条件に保留 PUBLISH_DONE を加えたが、`FinishRequestStream` による FIN は別経路である。
- `examples/moq-pub/src/pipeline.rs` は `publish_track` で PUBLISH 起点 publisher になっている
  (video / audio / catalog の 3 track)。保留 PUBLISH_DONE が生じないのは役割のためではなく、
  REQUEST_UPDATE に常に REQUEST_OK を返して拒否しないためである。
- Session 側では、PUBLISH_DONE を保留している間は peer FIN が遅延され、flush の
  `SendOnStream { fin: true }` が PUBLISH_DONE を書いてから送信方向が閉じる
  (`tests/test_session/subscription/publish_done.rs` の
  `pending_publish_done_flush_terminates_request_after_requester_fin` /
  `update_failed_publish_done_then_peer_fin_terminates_request` で固定)。したがって
  「peer FIN が保留 PUBLISH_DONE を失わせる」経路は現行実装では到達しない。

## 設計方針

現行実装では PUBLISH 起点 publisher の request に `FinishRequestStream` は発行されないため、以下は
example 側の防御 (現時点では到達しない経路) である。実施するかは現状の到達条件を踏まえて判断する。

- `FinishRequestStream` を受けたとき、当該 request の `Subscription::pending_publish_done` が `Some` なら
  FIN を保留する。判定は 0201 と同じく `Session::subscription` から必要な値だけを取り出す純関数にする。
- 保留した FIN の後始末を整理する。flush の `SendOnStream { fin: true }` が送信と FIN を行うため、保留した
  FIN は不要になる。保留したまま flush が起きなかった場合 (subscription が破棄された場合など) の扱いも
  決める。
- §6.4.2.2 の SHOULD (requester の FIN) と §9.5.1 / §9.9 の MUST / MUST NOT の優先関係を doc に残す。
- 保留が無い場合の `FinishRequestStream` の既存挙動は変えない。

## 完了条件

- 保留中の PUBLISH_DONE がある間は送信方向を FIN せず、PUBLISH_DONE が request stream へ書き出されること。
- 保留が無い場合は従来どおり FIN すること。
- 判定を単体テストで固定すること (I/O ハンドルが必要な配線部分はレビューで確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること

## 解決方法

コード変更なしで closed にする。磨き上げのレビューで、本 issue が扱う経路は現行実装では到達しない
と確定したためである。

- `pending_publish_done` が `Some` になるのは publisher 役に限られる。設定は
  `Session::send_request_error` の 1 経路のみで、そこへ至る `Session::send_err_for_subscription` は
  `subscription.my_role != TrackRole::Publisher` を拒否し、`Session::handle_err_for_subscription` も
  `my_role == TrackRole::Publisher` かつ `is_initiator_self()` を要求する。subscriber 役の拒否経路
  (`reject_request_update`) は `pending_publish_done` を立てない。
- `SessionEvent::FinishRequestStream` が発行されるのは subscriber 役に限られる。発行は
  `Session::recv_request_stream_closed` (`Session::is_local_requester` が `RequestKind::Publish` では
  常に false を返す) と `Session::handle_peer_publish_done` (`my_role != TrackRole::Subscriber` を
  拒否する) の 2 箇所である。
- したがって「保留 PUBLISH_DONE を持つ request に `FinishRequestStream` が発行される」経路は役割の
  不変条件で閉じている。example 側に FIN を保留する防御を足しても到達しないコードになる
  (example の配線を変えても成立せず、library の役割判定を変えない限り到達しない)。
- 保留 PUBLISH_DONE を持つ publisher 役で peer FIN が遅延されること自体は事実で、その組合せは
  到達する。遅延した FIN は flush の `SendOnStream { fin: true }` が PUBLISH_DONE を書いてから
  送信方向を閉じる。
- 上記の不変条件は既存テスト (`tests/test_session/request_stream.rs` の
  `publish_sender_survives_peer_fin_and_sends_publish_done` と
  `tests/test_session/subscription/handshake.rs` の `responder_does_not_get_finish_request_stream`)
  が固定済みで、本 issue を実装しても新しい保護は増えない。
- 本文の「Dropping send on already closed request stream」は現行のログ文言
  (`Dropping send on a request stream with no send half`) と異なる。`send_publish_done` のガード
  (§9.9 の MUST NOT) と、その拒否が warn として現れることは、moq-pub の後始末のコードコメントに
  記録した。
