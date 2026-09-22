# WebTransport の reset / STOP_SENDING でアプリケーションエラーコードを remap する

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-error-code-remap
- Polished: 2026-09-22

## 目的

draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) は、WebTransport のアプリケーションエラーコードを `WT_APPLICATION_ERROR` の範囲へ remap する MUST を定める。

> A WebTransport application MUST provide an error code for those operations.
> Since WebTransport shares the error code space with HTTP/3, WebTransport application errors for streams are limited to an unsigned 32-bit integer, assuming values between 0x00000000 and 0xffffffff.
> WebTransport implementations MUST remap those error codes into the error range reserved for WT_APPLICATION_ERROR, where 0x00000000 corresponds to 0x52e4a40fa8db, and 0xffffffff corresponds to 0x52e5ac983162.

> ```text
> first = 0x52e4a40fa8db
> last = 0x52e5ac983162
>
> def webtransport_code_to_http_code(n):
>     return first + n + floor(n / 0x1e)
>
> def http_code_to_webtransport_code(h):
>     assert(first <= h <= last)
>     assert((h - 0x21) % 0x1f != 0)
>     shifted = h - first
>     return shifted - floor(shifted / 0x1f)
> ```

現状は MOQT のエラーコードをそのまま QUIC の RESET_STREAM / STOP_SENDING に載せ、受信側も HTTP/3 のコードをそのまま MOQT のコードとして解釈する。MOQT のコードと HTTP/3 のコード空間が衝突し、reset の理由が peer に正しく伝わらない。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `WtSendStream::reset` は `s2n_quic::application::Error::new(error_code)` に MOQT のコード (例: `DataStreamResetReason::Cancelled` の 0x1) をそのまま渡す。
- 同 `WtRecvStream::stop_sending` も MOQT のコードをそのまま渡す。
- `examples/moqt-transport/src/transport.rs` の `SendStream::reset` / `RecvStream::stop_sending` は WebTransport 分岐で example 実装へそのまま渡す。QUIC 分岐は QUIC のコード空間なので remap しない。
- 受信側の `WtRecvStream::recv_chunk` は `s2n_quic::stream::Error::StreamReset { error, .. }` の値を `RequestStreamEnd::Reset { error_code }` にそのまま入れる。wire の値は remap 後の HTTP/3 コード空間の値であるため、MOQT のコードとしては誤っている。
- 呼び出し元は `examples/moqt-publisher/src/stream_writer.rs` の `SubgroupWriter::finish`、`examples/moqt-transport/src/moqt_client.rs` の `MoqtClient::stop_sending`、同 `MoqtClient` の `SessionEvent::ResetRequestStream` / `SessionEvent::StopSendingRequestStream` の処理である。
- 依存 `shiguredo_http3` の `shiguredo_http3::webtransport::ApplicationErrorCode` に `to_http3_code(u32) -> u64` と `from_http3_code(u64) -> Option<u32>` が実装済みで、§4.4 の予約コードポイント (`0x1f * N + 0x21`) のスキップも含む。example はこれを使っていない。

## 設計方針

- 送信時は `ApplicationErrorCode::to_http3_code` で remap した値を `s2n_quic::application::Error::new` に渡す。変換関数を `fn moqt_to_wt_code(code: u64) -> Result<u64>` として `webtransport.rs` に置き、`u32::try_from` で 32 ビットに収まることを確認してから `to_http3_code` に渡す。収まらない場合は黙って切り捨てずエラーにする。
  リポジトリ内の現行呼び出し元が渡す値は MOQT §12.5 の 0x0〜0x12 と `STREAM_*` だけなので到達しないが、`Session::reset_outgoing_data_stream_with_code` などの公開 API は任意の `u64` を受け、フィルタも `is_local_error_code` の 2 値だけなので、公開 API 経由で 32 ビット超のコードが渡され得る。
  その場合は remap をエラーにする (従来はそのまま wire に載っていたため、この防御は挙動変更になる)。この旨をコメントに書く。
- 受信時は `fn wt_to_moqt_code(h3_code: u64) -> Option<u32>` (`ApplicationErrorCode::from_http3_code` を呼ぶ) と、`RequestStreamEnd::Reset` に入れる値を決める `fn wt_reset_error_code(h3_code: u64) -> u64` を `webtransport.rs` に置く。
  `wt_reset_error_code` は remap できれば MOQT のコードを返し、できなければ `tracing::warn!` で生値をログに残したうえで wire の HTTP/3 コードをそのまま返す。`WtRecvStream::recv_chunk` はこの戻り値を `error_code` に入れる。`None` を返す値 (WT_SESSION_GONE などのプロトコルコードや予約コードポイント) は §4.4 の次の規定に従う。
  > If a RESET_STREAM or STOP_SENDING frame is received with an error code outside the range reserved for WT_APPLICATION_ERROR, the stream is still considered reset, but the error code is not mapped to a WebTransport application error code.
  > The WebTransport implementation SHOULD deliver this to the application as a stream reset with no application error code.

  `RequestStreamEnd::Reset` は `error_code: u64` を必須で持ち、「アプリケーションエラーコード無し」を表す値が無い。`Option<u64>` への型変更は `RequestStreamEnd` / `TerminationReason` の公開 API と 13 箇所以上の構築サイト (src / tests / examples) に波及するため本 issue の対象外とする。
  代わりに、`wt_reset_error_code` が remap できない場合に wire の HTTP/3 コードをそのまま返し、`tracing::warn!` で生値をログに残す。
  受信側で `error_code` に入る値は「WT_APPLICATION_ERROR の範囲 (`0x52e4a40fa8db` 以上 `0x52e5ac983162` 以下) だったものを remap した MOQT コード」と「それ以外の HTTP/3 コード (WT_SESSION_GONE = 0x170d7b68 などのプロトコルコードや予約コードポイント) をそのまま入れた値」の 2 種になる。
  MOQT §12.5 のコードは小さな varint (現行は 0x0〜0x12) なので前者とは区別できるが、後者の中には MOQT コードと区別できない値もある。この扱いと理由をコメントに残す。
- 変換は `examples/moqt-transport/src/webtransport.rs` の送信 / 受信それぞれ 1 関数に閉じ、`WtSendStream::reset` / `WtRecvStream::stop_sending` / `WtRecvStream::recv_chunk` から呼ぶ。`transport.rs` の WebTransport 分岐に変換を散らさない。
- §4.4 は WebTransport のデータストリームを reset する際に `RESET_STREAM_AT` を使う MUST も定めるが、s2n-quic が `RESET_STREAM_AT` に対応していない問題は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) が扱う。本 issue はエラーコードの remap のみを対象とし、reset フレームの種類は変更しない。
- `WtSession::close` が送る `WT_CLOSE_SESSION` capsule の Application Error Code は §4.4 の remap 対象ではない (32 ビットのアプリケーションコードを capsule がそのまま運ぶ)。この経路は変更しない。close 経路の値の切り捨て (`transport.rs` の `close(code as u32, ...)`) は [issues/0136](../issues/0136-bug-webtransport-session-termination.md) が扱う。
- QUIC 経路 (`moqt://`) の `SendStream::reset` / `RecvStream::stop_sending` / `RecvStream::receive_chunk` は remap しない。

## 完了条件

- WebTransport 経路で送るエラーコードが remap され、受信側で元の MOQT のコードに戻ることを `webtransport.rs` の `#[cfg(test)] mod tests` で固定するテストが追加されていること (I/O ハンドルを持たない変換関数を直接呼ぶ)
  - MOQT の登録コード (0x0 / 0x1 / 0x12) と §4.4 の変換の境界値 (0x1e / 0x1f / 0xffffffff) について、送信側の値が §4.4 の範囲 (`0x52e4a40fa8db` 以上 `0x52e5ac983162` 以下) に入り、受信側の変換で元の値に戻る往復テスト (0x1e は `to_http3_code` の結果が予約コードポイントにならないことも合わせて確認する)
  - 予約コードポイント (`0x1f * N + 0x21`) が受信側の変換で `None` になること
  - WT_APPLICATION_ERROR の範囲外の HTTP/3 コード (`WT_SESSION_GONE` = 0x170d7b68 と `0x12345678`) を受信側の変換関数へ直接渡すと `wt_to_moqt_code` が `None` を返し、`wt_reset_error_code` が同じ値をそのまま返すこと (`tracing::warn!` は `wt_reset_error_code` の中で出す)
  - 32 ビットに収まらない値 (`u64::MAX`) を送信側の変換関数へ直接渡すとエラーになること (切り捨てないこと)
- QUIC 経路のエラーコードが remap されないこと (変換関数は `webtransport.rs` に置き、`transport.rs` の QUIC 分岐から呼ばない)
- 実接続での send / recv の往復確認は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消後に行う (0135〜0137 と同じ扱い)
