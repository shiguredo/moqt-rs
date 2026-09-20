# WT_CLOSE_SESSION 受信後に CONNECT stream へ届いた追加データを H3_MESSAGE_ERROR で reset する

- Created: 2026-09-26
- Completed: 2026-09-30
- Branch: feature/fix-close-session-message-error-reset
- Polished: 2026-09-27

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
- `H3_MESSAGE_ERROR` は HTTP/3 のプロトコルエラーコードであり、§4.4 のアプリケーションエラーコードの remap (`moqt_to_wt_code` / `ApplicationErrorCode::to_http3_code`) に通さない。既存の `WtRecvStream::stop_sending` は Application 経路で remap
  するため、既存の `session_gone_code` (`StreamErrorCode::Protocol`) と同じ扱いで wire の値をそのまま渡す。値は example 側で再定義せず `shiguredo_http3::ErrorCode::MessageError` を使う。
- 接続は閉じない (ストリームの reset であり connection error ではない)。h3 の connection error の伝播は `issues/0138` が扱う。
- 実機確認は `issues/pending/0094` の解消後に行う。

## 完了条件

- `WT_CLOSE_SESSION` 受信後の追加データで CONNECT stream が `H3_MESSAGE_ERROR` で reset されること (判別を純関数として単体テストで固定する)
- 他の `StreamError` では reset しないこと
- セッション終了の検知と `WT_SESSION_GONE` でのストリーム中断の既存挙動が変わらないこと
- `make test` / `make clippy` / `make fmt` が通ること

## 解決方法

`examples/tokio-moq/src/webtransport_h3.rs` の CONNECT stream 受信タスクで、`WT_CLOSE_SESSION` 受信後の追加データによる `H3_MESSAGE_ERROR` を判別し、CONNECT stream の受信半へ `stop_sending` を送るようにした。

- `feed_connect_stream` の戻り値を `bool` から `Result<()>` に変更し、h3 層が返した `shiguredo_http3::Error` を呼び出し元で判別できるようにした
- `is_close_session_extra_data_error(error, session_state)` を追加した。依存 shiguredo_http3 は malformed capsule と `WT_CLOSE_SESSION` 受信後の追加データのどちらでも `Error::StreamError(MessageError)` を返すため、セッションが終了しているか (`session_policy(state).abort_streams`) と組み合わせて判別する。判別を純関数に切り出し、I/O ハンドルを持たないテストから固定できるようにした
- `abort_connect_stream_with_message_error` を追加した。`H3_MESSAGE_ERROR` は HTTP/3 のプロトコルエラーコードであり §4.4 のアプリケーションエラーコードの remap (`moqt_to_wt_code`) を通さないため、`StreamErrorCode::Protocol` で wire の値のまま渡す。値は example 側で再定義せず `shiguredo_http3::ErrorCode::MessageError` を使う
- 接続は閉じない (ストリームの reset であり connection error ではない)。セッション終了前の `MessageError` (malformed capsule / 未完成の capsule を残した FIN) は従来どおり session 終了として扱い、状態を `ClosedByPeer` へ移す

追加したテスト:

- `close_session_extra_data_error_is_detected_by_state_and_error`: 終了済みセッションの `StreamError(MessageError)` だけが対象になり、`Active` / `Draining` では対象にならないこと、他の `StreamError` (`FrameUnexpected` / `StreamNotFound`) は対象にならないことを固定した

未実施の確認:

- 実機確認は WebTransport セッションの確立が `issues/pending/0094` の解消待ちであるため行っていない。`WT_SESSION_GONE` でのストリーム中断の既存挙動は変更していない (`abort_connect_stream_read` はそのまま)

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
