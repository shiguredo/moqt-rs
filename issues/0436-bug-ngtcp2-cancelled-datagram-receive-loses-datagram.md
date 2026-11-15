# ngtcp2 バックエンドで datagram の受信待ちがキャンセルされると受信済み datagram が失われる

- Created: 2026-11-15
- Completed: 2026-11-15
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)

## 目的

`crates/tokio-moq/src/quic/ngtcp2.rs` の datagram 受信で、受信待ちの future が `select!` /
`timeout` で破棄されると、受信済みの datagram が失われる。datagram は再送されないため、
音声の実時間配送が欠落する。受信済み datagram を future の破棄で失わないようにする。

## 現状

- `Connection::receive_datagram` は `Command::ReceiveDatagram` をドライバへ送り、
  `oneshot::channel` の receiver を await する。
- ドライバは受信待ちの送信側を `datagram_waiters` に積み、datagram が届くと取り出して
  `let _ = waiter.send(Ok(data))` で渡す。送信に失敗した場合 (呼び出し側の future が既に
  破棄されている場合) は datagram を捨てる。
- 送信が成功した場合も、その直後に receiver が drop されると oneshot に格納された datagram が
  破棄される。受理ストリームで起きたのと同じ窓である。
- `crates/tokio-moq/src/transport.rs` の `StreamHandle::recv_datagrams` は
  `Connection::receive_datagram` を 1 回 await する。moq-sub の datagram 受信タスクは
  これをセッション終了の通知と `select!` で競わせており、キャンセルが起きる。
- 影響: `quic-ngtcp2` ビルドで音声を datagram で受けると、負荷時に datagram が欠落しうる。
  subgroup stream 経路には影響しない。stream 受理と open は別 issue で対応済み。

## 設計方針

- ストリーム受理と同じ共有待ち行列 (`StreamQueue<Vec<u8>>`) を使う。ドライバは受信した
  datagram をキューへ積むだけにし、アプリが取り出した時点で初めてキューから消す。
  受信待ちの future を破棄してもキューからは何も取り出さないため、datagram は失われない。
- `Command::ReceiveDatagram` は不要になるため削除する。`Connection` はキューを保持し、
  `receive_datagram` はキューから取り出すだけにする。
- 接続終了時はキューの閉鎖状態を立てて待ち手を起こす。閉鎖前に積まれた datagram は
  取り出せる。

## 完了条件

- datagram の受信待ちを破棄しても、受信済み datagram が失われないこと。
- 回帰テストがあること。受信待ちを 1 回だけ poll してから破棄し、そのあとに届いた
  datagram を次の受信で取り出せることを確認する。
- `cargo test -p tokio-moq --lib --no-default-features --features quic-ngtcp2` が通ること。
- `cargo test -p moq-sub --lib --no-default-features --features quic-ngtcp2 peer_harness_integration_tests`
  が通ること。
- `cargo clippy --workspace --all-targets -- -D warnings` と `cargo fmt --all -- --check` が通ること。

## 解決方法

datagram の受け渡しを `oneshot` から共有待ち行列 (`StreamQueue<Vec<u8>>`) に変えた。stream 受理
と同じ仕組みであり、受信待ちの future を破棄してもキューからは何も取り出さない。

- `crates/tokio-moq/src/quic/ngtcp2.rs` の `Connected` / `Connection` に
  `datagram_incoming: Arc<StreamQueue<Vec<u8>>>` を持たせ、`receive_datagram` はキューから
  取り出すだけにした。doc にキャンセル安全であることを明記した。
- ドライバは `ConnectionEvent::Datagram` で `datagram_incoming.push(data)` を呼ぶだけにした。
  `datagram_queue` / `datagram_waiters` / `Command::ReceiveDatagram` は削除した。
- 接続終了時は `finish()` で `datagram_incoming.close()` を呼び、待ち手を起こす。
- 回帰テスト `dropping_a_pending_datagram_receive_does_not_lose_the_datagram` を
  `crates/tokio-moq/src/peer_harness.rs` に追加した。受信待ちを 1 回だけ poll して保留のまま
  保持し、peer が datagram を送ったあとに値を受け取らず破棄する。その次の受信で同じ datagram を
  取り出せることを確認する。

変異確認: ドライバが datagram をキューへ積まず捨てるように変えると、この回帰テストが
5 秒の待ちで失敗する。修正後は成功する。

検証: `cargo test -p tokio-moq --lib --no-default-features --features quic-ngtcp2` (304 件) /
`cargo test -p moq-sub --lib --no-default-features --features quic-ngtcp2 peer_harness_integration_tests` /
`cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check`。
