# qmux-rs のドライバの終了条件の修正を取り込み、QMux 経路の接続とタスクの解放を解消する

- Created: 2026-10-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-qmux-driver-termination
- Polished: {YYYY-MM-DD}
- Updated: 2026-10-16

## 目的

QMux 経路で、自側の `close` の後も接続・タスク・TCP ソケットが解放されない問題を、qmux-rs のドライバの終了条件の修正を取り込んで解消する。qmux-rs 側は対応中である。

## 現状

- `examples/tokio-moq/src/qmux.rs` の `split_connection` は、peer 起動ストリームの受理と STOP_SENDING の観測を 3 つのタスクで行う。各タスクは `Arc<tokio_qmux::Connection>` を保持し、接続が閉じた場合 (`Connection::closed`) と、アプリが receiver を drop した場合 (`Sender::closed`) に終了する。
- qmux-rs (`c53b862`) のドライバは、自側の `close` の後は `transport_ended` が真になるまで動き続ける。`transport_ended` になるのは下位トランスポートの read EOF、flush の失敗、`Connection` がすべて drop された場合だけであり、`close` の後は `next_timeout` が `None` を返すため idle timeout も発火しない。また `close` は容量 1 のチャネルへの `try_send` のみで、失敗を返さない。
- 双方向 accept タスクは、`MoqtClient` 内の detached な受理タスク (`spawn_peer_bidi_accept_task`) が `BidiStreamAcceptor::Qmux` を保持し続けるため、アプリ側から receiver を drop できない。このため `Sender::closed` も `Connection::closed` も成立せず、接続・タスク・TCP ソケットが残る (単方向 accept と STOP_SENDING のタスクは receiver の drop で終了する)。
- 影響: moq-sub の GOAWAY 移行で旧 session の資源が残る。example はプロセス終了で消えるが、ライブラリとして使うと回収されない。ハングはしない (`close` は即座に返る)。
- qmux-rs の実装 (`crates/tokio-qmux/src/connection.rs`) では、`Driver` が自分の `command_tx` を保持するため `Wake::Command(None)` は到達せず、`close_request_rx` が `None` になること (= `Connection` の全 `Arc` の drop) がドライバ終了の唯一の引き金になっている。

## 設計方針

- qmux-rs の修正が入った rev へ依存を上げる。`examples/tokio-moq/Cargo.toml` の `shiguredo_qmux` と `tokio-qmux` を同じ rev に揃え、`Cargo.lock` と追随が必要な API 変更をまとめて反映する。
- ドライバが自側の `close` で (linger の上限つきで) 終了するようになったら、`spawn_accept_bidirectional` の `connection.closed()` でタスクが終了し、`Arc` の drop からドライバも終了する。`StreamHandle::close` の QMux の腕のコメントを実態に合わせる。
- qmux-rs が別の解 (タスクが `Weak` を持つ、`close` でドライバを終了させる API を足す等) を採る場合は、その API に合わせて `qmux.rs` のタスクの終了条件を書き換える。
- 修正をテストで固定する。client が `close` した後、または drop された後に、peer が下位トランスポートの EOF を観測する (またはドライバが終了する) ことを検証する。
- `peer_harness.rs` の `QmuxTestPeerServer` に、接続の終了を観測する手段を足す (現状は終了コードの記録を持たない)。

## 完了条件

- qmux-rs の修正を取り込んだ rev で、`close` の後にドライバと 3 つのタスクが終了し、下位トランスポートが解放されることがテストで固定されていること。
- `examples/tokio-moq/src/qmux.rs` の制約のコメントと `docs/missing-features.md` の QMux の記述が実態に追随していること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
