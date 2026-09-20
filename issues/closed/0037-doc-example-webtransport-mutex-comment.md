# example の WebTransport Mutex に関するコメントを実態に合わせる

- Created: 2026-09-10
- Completed: 2026-09-17
- Polished: 2026-09-17
- Branch: feature/fix-example-webtransport-mutex-comment

## 目的

`shiguredo-rust` 規約が求める「Mutex を使う理由」のコメントを実態と一致させ、誤った前提を残さない。

## 現状

`examples/moqt-transport/src/transport.rs` のコメントは「WebTransport では `WtSession` を `Arc<Mutex<...>>` で共有する。各操作は await をまたがず短時間で完結するため Mutex でよい」と説明する。しかし `StreamAcceptor::accept_recv_stream` の WebTransport 分岐は `session.lock().await` の後 `session.accept_uni_stream().await` を呼び、await
をまたいでロックを保持する (`WtSession::accept_uni_stream` は `uni_rx.recv().await` で待つ)。この間 `open_uni_stream` / `open_bi_stream` / `close` / `take_buffered_datagrams` がブロックされる。

## 設計方針

コメントを実態に合わせて修正する。または `WtSession::accept_uni_stream` を「ロック取得で 1 件取り出す」2 段構造にし、ロックを await に持ち越さない。後者の場合はデッドロック耐性が上がる。

## 完了条件

- コメントが実際のロック保持範囲と一致すること、またはロックを await に持ち越さない構造になっていること
- example のビルドと動作が維持されること

## 解決方法

設計方針の後者 (ロックを await に持ち越さない構造) を選び、あわせてコメントを実態に合わせた。

- `WtSession` の `uni_rx` / `bi_rx` を `Option<mpsc::Receiver<...>>` にし、
  `take_uni_receiver` / `take_bi_receiver` で receiver を取り出せるようにした
  (`examples/moqt-transport/src/webtransport.rs`)。取り出し後は
  `accept_uni_stream` / `accept_bi_stream` が `StreamClosed` を返す
- `StreamAcceptor::WebTransport` / `BidiStreamAcceptor::WebTransport` を struct variant にし、
  session と receiver の両方を持たせた (`examples/moqt-transport/src/transport.rs`)。
  `accept_recv_stream` / `accept_bidi_stream` は receiver の `recv()` をロック外で await する
- `MoqtClient::establish_wt` で receiver を取り出して acceptor へ渡すようにした
  (`examples/moqt-transport/src/moqt_client.rs`)
- `StreamHandle` のコメントを実態に合わせ、「送信系は await をまたいでも短時間で終わるため
  Mutex でよいが、受信ループの待機はロック外で行う」と説明し直した

これにより `accept` の待機中に `open_uni_stream` / `open_bi_stream` / `close` /
`take_buffered_datagrams` が `WtSession` のロックを待つことがなくなった。

検証:

- `cargo build --workspace` / `cargo test --workspace` /
  `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通る
- QUIC 直結 (`moqt://`) の動作を確認した。relay へ publisher (320x180@15fps) と
  subscriber を接続し、25 秒で 300 フレーム / 1200 音声チャンクを受信した
- WebTransport 経路は `issues/pending/0094` (s2n-quic の RESET_STREAM_AT 未実装) により
  セッションを確立できないため、example のビルドと QUIC 経路の動作で確認した
