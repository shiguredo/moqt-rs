# example の SendOnStream が fin 済み request への 2 度目の送信で失敗しないようにする

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-send-on-stream-after-fin
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-transport/src/moqt_client.rs` の `drain_events` が、fin 済みの request に対する後続の `SessionEvent::SendOnStream` で `no bidi stream for request_id` エラーを返して example を停止させる問題を解消する。

## 現状

`drain_events` の `SessionEvent::SendOnStream` 処理は `self.bidi_sends.remove(&request_id)` が `None` の場合に `TransportError::Internal("no bidi stream for request_id {request_id}")` を返す。`fin: true` のイベントを処理すると `bidi_sends` のエントリは再挿入されないため、同じ request への 2 度目の `SendOnStream` は必ずこのエラーになる。

同じファイルの `SessionEvent::ResetRequestStream` は「送信方向が既に FIN / RESET 済みの request では no-op にする」実装になっており、`SendOnStream` だけが hard error で扱いが揃っていない。

Session が同じ request へ 2 度目の `fin: true` を発行することを一時テストで確認した。手順は次のとおり。

- PUBLISH で確立した subscription (自側 subscriber) に、peer publisher 発の REQUEST_UPDATE を注入する
- REQUEST_UPDATE の Range Filter が自側 MAX_FILTER_RANGES を超えると `reject_request_update_range_filters` が `emit_request_error` で REQUEST_ERROR + `fin: true` を発行し、subscription は `Established` のまま残る
- 続けて pipelined に届いた 2 通目の REQUEST_UPDATE も同じ経路で拒否され、同じ request へ 2 度目の `REQUEST_ERROR + fin: true` が発行される

example は 1 度目の fin 処理で `bidi_sends` からエントリを消すため、2 度目の処理でエラーになる。なお draft-ietf-moq-transport-21 §9.5 (REQUEST_UPDATE) は失敗した複数の REQUEST_UPDATE を 1 つの REQUEST_ERROR に合体 (coalesce) してよいとしているため、2 度目の REQUEST_ERROR を送らない扱いは仕様に反しない。

0063 (WebTransport セッション確立) と 0067 (moqt-subscriber の runtime 構築失敗) は対象が異なる。

## 設計方針

- `bidi_sends` にエントリがない request への `SendOnStream` は no-op とし、warn ログで観測できるようにする (`ResetRequestStream` と同じ「既に閉じた request は no-op」の方針)。
- `fin: false` の通常送信がエントリなしで来る場合は Session 側の不整合である可能性があるため、ログに request id とメッセージ種別を残す。
- `closed_request_streams` による回収 (`cleanup_closed_requests`) と二重に `finish` しないことを確認する。
- Session 側で 2 度目の送信イベントを抑止するかは本 issue の対象外とする (必要なら別 issue で扱う)。

## 完了条件

- fin 済み request への 2 度目の `SendOnStream` で example がエラー終了しないこと
- 到達しない送信を no-op にしたことが warn ログで確認できること
- 初回の `SendOnStream` (`fin` の有無を問わず) の既存挙動が変わらないこと
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
