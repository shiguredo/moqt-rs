# WtH2SendStream の send / finish / reset が終了 watch と両 ready のときランダムで ConnectionClosed を返す

- Created: 2026-11-11
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-wt-send-connection-closed-race
- Polished: {YYYY-MM-DD}

## 目的

`crates/tokio-moq/src/webtransport_h2.rs` の `WtH2SendStream::send` / `finish` / `reset` は、操作の oneshot reply とセッション終端 watch を `tokio::select!` で競わせる。
reply が既に完成しているのに watch も発火済み (両 ready) のとき、非 biased の select は 50% で watch 分岐を選び `TransportError::ConnectionClosed` を返す。
driver は capsule を socket へ書き出す前に reply を送出するため、reject 系テストでは watch 発火時点で reply は常に完成しており (両 ready になり得る)、test task の poll タイミングと並列負荷が重なったとき `ConnectionClosed` を引く。
reply (またはそのエラー) が届いている場合に `ConnectionClosed` を返すのは誤報告であるため、両 ready 時の選択を決定的にする。

## 現状

- 2026-11-11 の flake 調査で `wt_h2_flow_control_rejects_equal_updated_max_stream_data` / `..._equal_bidi_remote_max_stream_data` が負荷下 (`--test-threads=96` + CPU ストレス) で `send に成功すること: ConnectionClosed` で失敗した (reject 1 テストあたり約 2/10 実行。無負荷では 10/10 PASS)。
- 非 biased の `tokio::select!` は毎 poll でランダムな起点から 1 個ずつ分岐を poll し、最初に `Ready` を返した分岐で return する。両 ready のとき 50% / 50% である。
- `send` (836 行付近) / `finish` (870 行付近) / `reset` (908 行付近) の 3 select が同型。操作 future (cmd_tx.send → rx.await) はいずれも先頭分岐に書かれている。
- reply は `flush_pending_sends()` で `wt_session.send_stream_data` が成功した直後に送出される (1719-1731 行付近)。socket への書き出し (`pump_and_flush`) はその後である。
- reject 系テストでは、server が client の data capsule を観測した後に violation capsule (非増加の `WT_MAX_*`) を返し、それを **client** 側の `feed` / `process` が検証して Err とし、client driver が `abort_session_with_wt_error` → `update_session_state(ConnectionFailed)` で watch を発火する (1819-1827 行付近)。
  WT over HTTP/2 では stream 開設は capsule を出さないため、server の観測は必ず client の data capsule 受信後に起きる。
  つまり driver 内の時間順は必ず「reply 送出 → violation → watch 発火」であり、watch 発火時点で reply は既に oneshot に在る。
- `tokio::select!` の `biased` モードは宣言順に poll し、先頭分岐が `Ready` なら後続を poll しない。分岐は現行で既に操作側が先頭のため、`biased;` の追加だけで両 ready 時に reply 優先が決定的になる。
- 0421 で解決した受理系 4 分岐 (server の即終了) とは別物である。reject 系では server の即終了 (RST 送出後の return) が観測の主題であり、0421 の「server を生かす」パターンは適用できない。

## 設計方針

- `send` / `finish` / `reset` の 3 select に `biased;` を追加する。両 ready 時に操作分岐 (reply) を優先する。reply が `Err(StreamReset / StreamClosed 等)` でも watch の `ConnectionClosed` より精緻なため、詳細エラーを返す。
- 操作 future が `Pending` の間は watch 分岐が poll されるため starvation は起きない。操作分岐が選ばれたときの内部 2 段 await は select の futures タプルに状態が永続するため cancel-safety の問題は無い。
- driver が死んでいる経路 (`cmd_tx.send` 失敗 / watch 未発火 / watch 発火済みで reply 未送出) の意味論は変えない (いずれも `ConnectionClosed`)。
- `send` の doc の「driver が `WT_STREAM` capsule へ変換して CONNECT ストリームに載せるまで待つ」を「capsule 化の受理まで待つ。socket への書き出し完了は保証しない」に整える (reply の送出タイミングと doc の不一致の解消)。

## 完了条件

- reply が既に完成している状態で watch が発火したとき、`send` / `finish` / `reset` が reply の結果 (Ok または詳細エラー) を返すこと (`ConnectionClosed` を返さないこと)。
- 負荷の高い並列実行 (`cargo test -p tokio-moq --lib flow_control` を通常設定 15 回 + 並列負荷 15 回、またはそれと同等の反復) で reject 系のうち送信を伴う 4 テスト (`wt_h2_flow_control_rejects_equal_uni_max_stream_data` / `..._equal_bidi_remote_max_stream_data` / `..._equal_bidi_local_max_stream_data`
   / `..._equal_updated_max_stream_data`) が失敗しないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
