# PUBLISH 起点 publisher の保留 PUBLISH_DONE が request stream の FIN で失われる

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-moqt-client-publish-done-after-finish-request-stream
- Polished: {YYYY-MM-DD}

## 目的

publisher 役で REQUEST_UPDATE を拒否したときの PUBLISH_DONE (`UPDATE_FAILED`) は、open 中の
outgoing data stream が無くなった時点で送る (draft-ietf-moq-transport-22 §9.5.1 (Updating
Subscriptions) の MUST)。§9.9 (PUBLISH_DONE) も PUBLISH_DONE を送るまで subscription の状態を破棄しない
ことを MUST NOT で求めている。自側が PUBLISH を送った publisher (= requester) は、peer が request
stream の送信方向を FIN すると `SessionEvent::FinishRequestStream` を受け取り、それに従って自側の送信
方向を FIN する (§6.4.2.2 (Graceful Request Stream Closure) の SHOULD)。保留中の PUBLISH_DONE がある間に
FIN すると、その後の flush が `SendOnStream` を発行しても送信半が台帳に無く「Dropping send on already
closed request stream」で捨てられ、§9.5.1 の MUST を果たせない。

## 現状

- `src/session/core.rs` の `Session::recv_request_stream_closed(RequestStreamEnd::Fin)` は、自側が
  requester のとき `SessionEvent::FinishRequestStream` を発行する。
- `examples/tokio-moq/src/moqt_client.rs` の `drain_events` の `SessionEvent::FinishRequestStream` 分岐は
  `bidi_sends` から送信半を取り出して `finish()` する。`Subscription::pending_publish_done` を見ていない。
- `Session::maybe_flush_pending_publish_done` は open 中の outgoing data stream が無くなると
  `SendOnStream { fin: true }` を積むが、送信半が既に無い場合は example が warn を出して捨てる
  (同じ `drain_events` の `SendOnStream` 分岐の else 経路)。
- 0201 で回収の条件に保留 PUBLISH_DONE を加えたが、`FinishRequestStream` による FIN は別経路である。
- 現在の examples (moq-pub は SUBSCRIBE の responder、moq-sub は subscriber) は PUBLISH 起点 publisher に
  ならないため到達しない。`Session::send_publish` は公開 API であり、この役割を example に足すと到達する。

## 設計方針

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
