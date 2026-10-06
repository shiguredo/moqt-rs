# WebTransport の datagram 受信タスクがセッション終了後も動き続ける

- Created: 2026-09-26
- Completed: 2026-10-06
- Branch: feature/fix-webtransport-datagram-task-lifetime
- Polished: 2026-09-27

## 目的

WebTransport の datagram 受信タスクがセッション終了を観測しても終わらない問題を解消する。draft-ietf-webtrans-http3-16 §6 はセッション終了後に新しい datagram を送ることを禁じるが、受信側も終了後のポーリングを続ける必要はない。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `WtClient::connect` は `config.receive_datagrams` が真のとき datagram 受信タスクを spawn する。spawn は CONNECT レスポンス待ちの前であり、セッション確立の成否をまたがない。タスクは `s2n_quic::connection::Handle::datagram_mut` を呼び続け、受信が無いときは `tokio::time::sleep` で間隔を空ける。
- タスクはセッション状態 (`WtSessionState` の `watch`) を観測しないため、セッション終了後も残る。`watch::Sender` の clone を保持し続けるため、`wait_until_terminated` の sender drop 経路も塞ぐ。
- 受信した datagram を h3 層へ流す `feed_datagram` は終了後も `Ok` を返すため、タスク自身は終了を検知できない。

## 設計方針

- タスクの受信ループに `wait_until_terminated` を `tokio::select!` の分岐として足し、セッション終了を観測したら抜ける。
- 抜けたあとは datagram を読まない (MOQT 層は `take_buffered_datagrams` が返す `ConnectionClosed` で終了を検知する)。
- I/O ハンドルを持つためタスクの終了そのものは単体テストで固定できない。終了判定は既存の `session_policy` / `wait_until_terminated` に集約されており、本修正で新たに現れる純関数は無い。追加テストは要さず、配線はレビューと実機で確認する。
- セッションが確立しないまま `WtClient::connect` がエラーを返した場合も、接続が生きている限りタスクは残る。この残存は本修正の対象外とし、接続が終了したときに `datagram_mut` のエラーで break する既存経路で終える。

## 解決方法

`examples/tokio-moq/src/webtransport_h3.rs` の datagram 受信タスクがセッション終了を観測して
終了するようにした。

- datagram の受信キューが空のときの待機を `tokio::select!` にし、`wait_until_terminated` が
  返ったらループを抜けるようにした。セッション終了を観測したタスクは datagram を読まなくなり、
  タスクが持つ `watch::Sender` の clone も drop されるため、他の待機側の sender drop 経路を
  塞がなくなる
- 終了の観測はキューが空のときだけ行う。キューに datagram が残っている間に抜けると、終了直前に
  peer が送った Object Datagram を落とすことになり、`resolve_buffered_datagrams` の
  「セッション終了を検知しても同じバッチで取り出した datagram は捨てない」という方針に反するため
- datagram の受信内容の扱い (h3 層への feed と `process_h3_outcome`) は変えていない
- I/O ハンドルを持つタスクのため終了そのものは単体テストで固定できない。終了判定はテスト済みの
  `session_policy` / `wait_until_terminated` に集約されており、本修正で新たに現れる純関数は
  無いため追加テストは置かず、配線をレビューで確認した
- 検証: `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` / `prek run --all-files` が通ることを確認した

## 完了条件

- セッション終了を観測した datagram 受信タスクが終了すること
- セッション終了後に datagram の受信を続けないこと
- datagram の受信内容の扱いを変えないこと (WebTransport 接続の subscriber への Object Datagram 配送は `issues/pending/0010-bug-webtransport-datagram-receive.md` の範囲であり、0175 では扱わない)
- `make test` / `make clippy` / `make fmt` が通ること
