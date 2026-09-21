# WebTransport ストリームの session ID 不正で H3_ID_ERROR を送る

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-session-id-validation
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 §4 (WebTransport Features) は、session ID が client-initiated bidirectional stream ID に対応しないストリームを受信した場合に H3_ID_ERROR で接続を閉じる MUST を定める。

> Session IDs are derived from the stream ID of the CONNECT stream that established the session and therefore MUST always correspond to a client-initiated bidirectional stream, as defined in Section 2.1 of [RFC9000].
> If an endpoint receives a session ID on a unidirectional stream, bidirectional stream, or datagram that does not correspond to a client-initiated bidirectional stream ID, the endpoint MUST close the connection with an H3_ID_ERROR error code.
> Session IDs that correspond to closed sessions are not considered invalid for the purposes of this check; endpoints handle data for closed sessions as described in Section 6.

現状は不正なストリームを黙って捨てて接続を閉じないため、違反した peer を検知できず、MOQT 層には何も起きていないように見える。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `route_uni_stream` は `classify_uni_stream_checked` の結果を `Err(_) => return` で捨てる。依存 `shiguredo_http3` の `StreamHeaderDecodeError` は `InvalidSessionId` を「呼び出し側は H3_ID_ERROR で接続を閉じる」と定義しており、`SessionIdOutOfRange` (QUIC のストリーム ID 上限超え) も同じ扱いを要する。example はこの指示に従っていない。
- `route_uni_stream` は `ClassifiedUniStream::WebTransport { data_offset, .. }` で `session_id` を捨てる。自セッション以外の session ID を持つ単方向ストリームも `uni_tx` へ渡り、MOQT の data stream として扱われる。
- `route_bi_stream` も `Err(_) => return` でデコードエラーを捨てる。こちらは `StreamHeader::decode_bidirectional_checked` の結果を `session_id` と照合しており、他セッション向けのストリームは HTTP/3 層へ渡している。
- どちらのルーティングタスクも接続を閉じる経路を持たない。タスクは `WtClient::connect` から spawn されるため、`WtSession` のエラー経路にも現れない。

## 設計方針

- ストリームヘッダーのデコードエラーのうち接続エラーに相当するもの (`InvalidSessionId` / `SessionIdOutOfRange`) を接続エラーとして扱い、HTTP/3 の CONNECTION_CLOSE を H3_ID_ERROR で送る。
  h3 層は Sans I/O のため CONNECTION_CLOSE の送出は I/O 層 (s2n-quic) が行う。`s2n_quic::connection::Handle::close` に HTTP/3 のエラーコード (H3_ID_ERROR = 0x108) を application error code として渡す。
- エラーコードは example 側で数値を再定義せず、`shiguredo_http3::Error::ConnectionError(ErrorCode)` の `ErrorCode::code()` を使う。h3 層が公開しているエラー判定を唯一の出典にする。
- `route_uni_stream` / `route_bi_stream` が接続を閉じられるように、`WtClient::connect` が保持する `s2n_quic::connection::Handle` を clone して渡す。
- 単方向ストリームでも `ClassifiedUniStream::WebTransport { session_id, .. }` の `session_id` を `WtSession::session_id` と照合し、他セッション向けのストリームを MOQT 層へ渡さない。双方向ストリームと同じ扱いにそろえる。閉じたセッションの session ID は §4 の末尾により不正ではないため、自セッションでないことだけを理由に接続を閉じない。
- デコード結果から次の動作 (継続 / 破棄 / 接続クローズ) を決める判定を純関数に切り出し、spawn されたタスクの外からテストできるようにする。`BufferTooShort` は追加受信待ちの継続、双方向ストリームの `InvalidFormat` は現状の扱い (HTTP/3 層の管轄) を変えない。

## 完了条件

- 不正な session ID のストリームを受けたときに接続が H3_ID_ERROR で閉じることを確認できること。確認方法は次のとおり。
  - 単体テスト: デコード結果から動作を決める判定関数に `InvalidSessionId` / `SessionIdOutOfRange` を与え、H3_ID_ERROR での接続クローズが選ばれること。`BufferTooShort` が継続になること
  - 実機: `session_id % 4 != 0` の WebTransport 単方向 / 双方向ストリームを送るテストクライアントを用意し、example 側の接続が閉じることを `RUST_LOG=debug` のログで確認する。WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う
- 他セッション向けの単方向ストリームが MOQT 層 (`uni_tx`) へ渡らないこと (単体テストまたは再現手順で確認)
- 正常な session ID のストリームの扱いが変わらないこと (既存のルーティングが通ること)
