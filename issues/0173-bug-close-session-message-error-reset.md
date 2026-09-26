# WT_CLOSE_SESSION 受信後に CONNECT stream へ届いた追加データを H3_MESSAGE_ERROR で reset する

- Created: 2026-09-26
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-close-session-message-error-reset
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 §6 (Session Termination) の MUST を満たす。

> If any additional stream data is received on the CONNECT stream after receiving a WT_CLOSE_SESSION capsule, the stream MUST be reset with code H3_MESSAGE_ERROR.

## 現状

- `examples/moqt-transport/src/webtransport.rs` の CONNECT stream 受信タスクは、h3 層の `feed_stream` が `Err(StreamError(MessageError))` を返した場合に `tracing::warn!` を出して受信を止め、セッションを終了として扱うだけで、CONNECT stream を reset しない。
- 同ファイルの `WtSession::close` の doc コメントに、`WT_CLOSE_SESSION` 受信後に追加データを受信した場合の `H3_MESSAGE_ERROR` reset が非対応であると明記されている。
- CONNECT stream の受信半 (`s2n_quic::stream::ReceiveStream`) は受信タスクが保持しているため、`stop_sending` を送る余地はある。
- 依存 `shiguredo_http3` は `Error::StreamError(MessageError)` を返す経路として、malformed capsule と、`WT_CLOSE_SESSION` 受信後の追加データの 2 つを持つ。

## 設計方針

- h3 層のエラーが「`WT_CLOSE_SESSION` 受信後の追加データ」によるものかを判別し、その場合は CONNECT stream の受信半へ `H3_MESSAGE_ERROR` で `stop_sending` を送る。
- 判別は純関数に切り出す (`shiguredo_http3::Error` の variant と、セッションが終了しているかを入力にする)。
- 接続は閉じない (ストリームの reset であり connection error ではない)。h3 の connection error の伝播は `issues/0138` が扱う。
- 実機確認は `issues/pending/0094` の解消後に行う。

## 完了条件

- `WT_CLOSE_SESSION` 受信後の追加データで CONNECT stream が `H3_MESSAGE_ERROR` で reset されること (判別を純関数として単体テストで固定する)
- 他の `StreamError` では reset しないこと
- セッション終了の検知と `WT_SESSION_GONE` でのストリーム中断の既存挙動が変わらないこと
- `make test` / `make clippy` / `make fmt` が通ること
