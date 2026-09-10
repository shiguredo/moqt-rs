# example の WebTransport Mutex に関するコメントを実態に合わせる

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
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
