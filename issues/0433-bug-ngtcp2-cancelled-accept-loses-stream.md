# ngtcp2 バックエンドで accept 待ちがキャンセルされると受理済みの stream が失われる

- Created: 2026-11-14
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)

## 目的

`crates/tokio-moq/src/quic/ngtcp2.rs` のストリーム受理で、待ち手 (oneshot の receiver) が
キャンセルされると受理済みの stream が捨てられ、その後の accept で二度と取得できなくなる。
キャンセル安全にし、stream を取りこぼさないようにする。

## 現状

- `crates/tokio-moq/src/quic/ngtcp2.rs` の `StreamAcceptor::accept_receive_stream` /
  `accept_bidirectional_stream` は、`Command::AcceptUni` / `Command::AcceptBidi` をドライバへ送り、
  `oneshot::channel` の receiver を await する。
- 呼び出し側はこの future を `tokio::time::timeout` や `tokio::select!` で破棄しうる。
  `crates/tokio-moq` の `crates/tokio-moq/src/moqt_client.rs` と `crates/tokio-moq/src/qmux.rs` は
  datagram 受信や制御ストリームと受理を `select!` で競わせており、`crates/moq-pub` /
  `crates/moq-sub` の integration test でも受理待ちに `timeout` を掛けている。
- receiver が drop されると oneshot の `send` は失敗し、`stream` を返す。現行実装は次の 2 箇所で
  その戻り値を捨てている。
  - `DriverState` のストリーム到着処理: `bidi_waiters` / `uni_waiters` から待ち手を 1 つ取り出して
    `let _ = waiter.send(Ok(stream));` で送る。失敗すると stream は捨てられる。
  - `Command::AcceptUni` / `Command::AcceptBidi`: キューから stream を取り出して
    `let _ = reply.send(Ok(stream));` で送る。失敗すると stream は捨てられる。
- 影響: キャンセルされた accept が 1 回あると、その後に到着した stream (またはキューにあった
  stream) が失われ、購読や FETCH の応答 stream が永久に届かない。上位は data stream の待ちで
  タイムアウトするまで気づけない。
- `crates/tokio-moq/src/quic/s2n.rs` と `crates/tokio-moq/src/quic/tobi_backend.rs` の
  `StreamAcceptor` はバックエンドの受理 API をそのまま await するため、この問題は ngtcp2
  バックエンド固有である。
- 発端: `crates/moq-sub` の integration test が `quick-ngtcp2` ビルドで失敗し、受理待ちの
  キャンセルで stream が失われていることを確認した。

## 設計方針

- キャンセルされた待ち手へ送れなかった stream は捨てずに次の待ち手へ渡し、待ち手が尽きたら
  キューへ戻す。待ち手のキューには cancel 済みの receiver が残りうるため、1 回の失敗で諦めず
  ループして次の待ち手へ渡す。
- `Command::AcceptUni` / `AcceptBidi` でキューから取り出した stream を送れなかった場合は、
  キューへ戻す。到着済み stream の順序を保つため、戻す位置はキューの先頭 (`push_front`) にする。
- `Command` は `oneshot::Sender<std::result::Result<SendStream, QuicError>>` のように `Ok` だけを
  送るため、`send` の失敗は `Err(Ok(stream))` の形でしか起きない。`Err(Err(_))` は到達しないが、
  網羅性のために分岐を書き、防御であることをコメントに残す。
- 待ち手がキャンセルされたことを契機に stream を別の待ち手へ渡しても、
  `Command::AcceptUni` / `AcceptBidi` の登録順序 (FIFO) は保たれる。譲る相手は登録順で先頭の
  待ち手である。
- 回帰テストを `crates/tokio-moq/src/peer_harness.rs` の `#[cfg(test)] mod tests` に置く。
  受理待ちを `tokio::time::timeout` で破棄した後に peer が stream を開き、次の accept で
  取得できることを確認する。`peer_harness` は `test-support` feature で公開しているため、
  `crates/moq-pub` / `crates/moq-sub` の統合テストからも同じ経路を通せる。

## 完了条件

- accept 待ちがキャンセルされた後でも、その後に到着した stream を次の accept で取得できること。
- accept コマンド送信後に呼び出し側がキャンセルした場合も、キューにあった stream が失われないこと。
- `crates/tokio-moq/src/peer_harness.rs` に回帰テストがあること。
- `cargo test -p tokio-moq --lib` が通ること。
- `cargo test -p moq-sub --lib --no-default-features --features quic-ngtcp2` が通ること。
- `cargo clippy -p tokio-moq -p moq-sub --all-targets -- -D warnings` と
  `cargo fmt --all -- --check` が通ること。
