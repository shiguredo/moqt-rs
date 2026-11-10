# WebTransport over HTTP/2 の WT_MAX_DATA 増加テストの競合を取り除く

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-wt-h2-flow-control-flaky-test
- Polished: {YYYY-MM-DD}

## 目的

`crates/tokio-moq/src/webtransport_h2.rs` のテスト `wt_h2_flow_control_accepts_increased_max_data` が負荷の高い並列実行で失敗し、develop の CI (`cargo test --workspace`) と pre-push フックを止める。
テスト用 server が受信データを観測した時点で終了し、client の送信の完了と競合しているため、競合を取り除いて決定的に通るようにする。

## 現状

- 2026-11-09 の develop の CI (0343 の push) で `send に成功すること: ConnectionClosed` の panic により失敗した。テストは c46e987 (0406) で追加された。
- ローカルでも `cargo test -p tokio-moq --lib` を 15 回実行して 1 回失敗した。単体で 40 回実行した場合は失敗しない。
- client 側の `prepare_flow_control_client` は `FlowControlProbe::MaxDataIncrease` で `open_uni_stream` → `send(b"hello")` → `finish()` を順に待ち、それぞれ `expect` で成功を要求する。
- server 側の `run_flow_control_server` は `Event::DataReceived` の処理で `WtEvent::StreamData` を `received` に積み、`received` が空でなければ `FlowControlProbe::MaxDataIncrease` / `StreamDataIncreaseBidiLocal` で観測結果を送って `return Ok(())` する。
  server task が終わると接続が閉じるため、client の `send` / `finish` の future が完了する前に `ConnectionClosed` を返し得る。
- `WtEvent::StreamData` は `fin: bool` を持ち、受信側はストリームの終端を観測できる (`shiguredo_http2::webtransport::WtEvent`)。

## 設計方針

- server は受信データで即座に終了せず、対象ストリームの `WtEvent::StreamData { fin: true }` を受け取ってから観測結果を送って終了する。client の `finish()` が送った FIN を server が受け取った後なので、client の `send` / `finish` は接続の終了と競合しない。
- 同じく受信データで即座に終了する `FlowControlProbe::StreamDataIncreaseBidiLocal` の分岐も、client 側の送信手順 (`prepare_flow_control_client`) を確認したうえで同じ規則にそろえる。
- テストの検証内容 (増加した WT_MAX_DATA の範囲で client が送信できること) は変えない。

## 完了条件

- `wt_h2_flow_control_accepts_increased_max_data` (と同じ規則にそろえた分岐を使うテスト) が、server の終了と client の送信の完了の競合に依存しないこと。
- `cargo test -p tokio-moq --lib` を繰り返し実行しても当該テストが失敗しないことを確認すること (目安として負荷下で 30 回以上)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
