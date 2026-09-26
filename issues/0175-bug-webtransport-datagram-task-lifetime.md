# WebTransport の datagram 受信タスクがセッション終了後も動き続ける

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-datagram-task-lifetime
- Polished: {YYYY-MM-DD}

## 目的

WebTransport の datagram 受信タスクがセッション終了を観測しても終わらない問題を解消する。draft-ietf-webtrans-http3-16 §6 はセッション終了後に新しい datagram を送ることを禁じるが、受信側も終了後のポーリングを続ける必要はない。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `WtClient::connect` は `config.receive_datagrams` が真のとき datagram 受信タスクを spawn する。タスクは `s2n_quic::connection::Handle::datagram_mut` を呼び続け、受信が無いときは `tokio::time::sleep` で間隔を空ける。
- タスクはセッション状態 (`WtSessionState` の `watch`) を観測しないため、セッション終了後も残る。`watch::Sender` の clone を保持し続けるため、`wait_until_terminated` の sender drop 経路も塞ぐ。
- 受信した datagram を h3 層へ流す `feed_datagram` は終了後も `Ok` を返すため、タスク自身は終了を検知できない。

## 設計方針

- タスクの受信ループに `wait_until_terminated` を `tokio::select!` の分岐として足し、セッション終了を観測したら抜ける。
- 抜けたあとは datagram を読まない (MOQT 層は `take_buffered_datagrams` が返す `ConnectionClosed` で終了を検知する)。
- I/O ハンドルを持つためタスクの終了そのものは単体テストで固定できない。判断を純関数に切り出せる範囲はテストし、配線はレビューと実機で確認する。

## 完了条件

- セッション終了を観測した datagram 受信タスクが終了すること
- セッション終了後に datagram の受信を続けないこと
- datagram の受信内容の扱い (pending の 0010 の範囲) を変えないこと
- `make test` / `make clippy` / `make fmt` が通ること
