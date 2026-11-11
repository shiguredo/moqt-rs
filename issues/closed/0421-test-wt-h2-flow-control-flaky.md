# WebTransport over HTTP/2 の flow control テストの競合を取り除く

- Created: 2026-11-10
- Completed: 2026-11-11
- Branch: feature/fix-wt-h2-flow-control-flaky-test
- Polished: 2026-11-11

## 目的

`crates/tokio-moq/src/webtransport_h2.rs` の flow control テストが負荷の高い並列実行で失敗し、develop の CI (`cargo test --workspace`) と pre-push フックを止める。
テスト用 server が観測した時点で即座に終了し、client の送信の完了と競合しているため、競合を取り除いて決定的に通るようにする。

## 現状

- 2026-11-09 の develop の CI (0343 の push) で `wt_h2_flow_control_accepts_increased_max_data` が `send に成功すること: ConnectionClosed` の panic により失敗した。テストは c46e987 (0406) で追加された。
- ローカルでも `cargo test -p tokio-moq --lib` を 15 回実行して 1 回失敗した。単体で 40 回実行した場合は失敗しない。
- `run_flow_control_server` は観測結果を `observed` (oneshot) に送って直ちに `return Ok(())` する分岐が 4 つある:
  - 受信データを観測して即 return する `FlowControlProbe::MaxDataIncrease` / `FlowControlProbe::StreamDataIncreaseBidiLocal` 分岐 (テスト `wt_h2_flow_control_accepts_increased_max_data` / `wt_h2_flow_control_accepts_increased_max_stream_data`)
  - 2 本目の stream 開設を観測して即 return する `FlowControlProbe::StreamsIncreaseUni` / `FlowControlProbe::StreamsIncreaseBidi` 分岐 (テスト `wt_h2_flow_control_accepts_increased_max_streams` / `wt_h2_flow_control_accepts_increased_bidi_max_streams`)
- server task が `return Ok(())` で終わると io が drop して接続が閉じる。client driver は EOF を検知して `WtSessionState::ConnectionFailed` を設定し、未完了の送信を abort する (`abort_streams`)。
- client 側の `prepare_flow_control_client` は送信の完了を `tokio::select!` (oneshot の reply と終了 watch の競合) で待つ。driver は reply を socket への書き出し前に送出するため、test task の再ポーリングが接続閉鎖の伝播より遅れると、
  両分岐同時 ready の状態から `Err(ConnectionClosed)` が選ばれ `send に成功すること: ConnectionClosed` で panic する (同じ構造は `finish` にもあるため、単に server の終了を遅らせただけでは panic の出力先が入れ替わるだけである)。
- `WtEvent::StreamData` は `fin: bool` を持つが、fin を待つ設計は使わない。`FlowControlProbe::StreamDataIncreaseBidiLocal` の client は FIN を送らない手順 (bidi receiver → recv → `send(b"x")` のみ) であり、fin 待ちにすると server が永久に待って確定的に失敗するためである。

## 設計方針

- server は観測結果を送った後も接続を自発的に閉じない。4 分岐すべての `return Ok(())` をやめ、観測結果の送信後に通常の event ループを続けてテスト側の接続終了を待つ。
- 観測結果の送信は 1 回だけにする。4 分岐の送信箇所は 1 分岐につき 2 つある (poll_event 内と `pump_server_output` 後) ため、return をやめると同一観測で 2 回送り得る。`observed` を `Option` にして take する等のガードを入れ、再評価では 2 回目を送らない。
- テスト側の終了手順を変える。現行の `assert_flow_control_observation` は観測受信 → assert → `server.await` の順で、`session` は `server.await` の後の drop である。server が自発的に閉じない設計では依存が逆になり `server.await` が解決しない (hang)。観測受信と assert の後、`server.await` の前に session を drop して接続を閉じる手順に変える。
- server 側は観測送信後の接続終了を正常終了として扱う。session drop は CONNECT stream に END_STREAM を送らず io drop による TCP 閉鎖 (EOF) として届くため、`next_event` は `TransportError::ConnectionClosed` を返す。観測結果の送信後はこの EOF (観測済み) をエラーにせず正常終了する (`server.await` の結果の expect が panic しない)。
- client 側の送信手順 (`prepare_flow_control_client`) は現状のまま変えない (`FlowControlProbe::StreamDataIncreaseBidiLocal` に `finish()` を追加する変更もしない。拒否テスト用の `FlowControlProbe::StreamDataBidiLocal` と共用であり、server が先に閉じない設計では競合源を増やす必要が無いため)。
- テストの検証内容 (増加した WT_MAX_DATA / WT_MAX_STREAM_DATA / WT_MAX_STREAMS の範囲で client が送信・開設できること) は変えない。

## 完了条件

- flow control の 4 分岐 (`MaxDataIncrease` / `StreamDataIncreaseBidiLocal` / `StreamsIncreaseUni` / `StreamsIncreaseBidi`) を使うテストが、server の終了と client の送信の完了の競合に依存しないこと。server が観測結果送信後に自発的に接続を閉じないこと (観測結果の送信は 1 回だけであること)。
- テスト側が観測受信と assert の後に session を drop してから `server.await` を行う手順になり、server task が観測送信後の EOF を正常終了として扱うこと (hang / panic が無いこと)。
- `cargo test -p tokio-moq --lib` を繰り返し実行しても当該テストが失敗しないことを確認すること (目安として負荷下で 30 回以上)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `crates/tokio-moq/src/webtransport_h2.rs` の `run_flow_control_server` を変更した。受理系 4 分岐 (`MaxDataIncrease` / `StreamDataIncreaseBidiLocal` / `StreamsIncreaseUni` / `StreamsIncreaseBidi`) の `return Ok(())` をやめ、観測結果の送信後に通常の event ループを続けてテスト側の接続終了を待つ。観測結果の送信は `observed` を `Option<oneshot::Sender>` にして `take()` するガードで 1 回だけにした (同一観測の再評価箇所が複数あるため)。後続の再評価では `observed` が `None` になるため 2 回目の送信は起きない。
- `next_event` のエラーを、観測結果を送り終えた後 (`observed` が `None` のとき) は正常終了 (`Ok(())`) として扱うようにした。テストは観測受信後に session を drop して接続を閉じるため、EOF が `TransportError::ConnectionClosed` として届くが (session drop は CONNECT stream に END_STREAM を送らず io drop による TCP 閉鎖で届く)、観測済みの接続終了をエラーにしない。
- テスト側の `assert_flow_control_observation` を変更した。観測受信と assert の後、`server.await` の前に session を drop して接続を閉じる手順にする。server が自発的に閉じない設計では、session drop (旧手順では `server.await` の後) と server task の終了待ちの依存が逆になり解決しないためである。client 側の送信手順 (`prepare_flow_control_client`) は現状のまま変えない (`FlowControlProbe::StreamDataIncreaseBidiLocal` に `finish()` を追加する変更もしない。拒否テスト用の `FlowControlProbe::StreamDataBidiLocal` と共用であり、server が先に閉じない設計では競合源を増やす必要が無いため)。
- reject 系分岐 (`StreamReset` / `StreamClosed` / `ConnectionError` / ループ末端) も `observed.take()` を経由する形に統一した (挙動は不変。旧実装の「send 1 回して return」と同一意味論)。
- 検証: `cargo test -p tokio-moq --lib flow_control` を通常設定 15 回 + 並列負荷 15 回 + 修正後 20 回の計 50 回実行して 0 失敗 (完了条件の 30 回以上を満たす)。`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` / `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` も通る。
