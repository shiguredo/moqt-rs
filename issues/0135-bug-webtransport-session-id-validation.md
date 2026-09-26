# WebTransport ストリームの session ID 不正で H3_ID_ERROR を送る

- Created: 2026-09-21
- Completed: 2026-09-25
- Branch: feature/fix-webtransport-session-id-validation
- Polished: 2026-09-22

## 目的

draft-ietf-webtrans-http3-16 §4 (WebTransport Features) は、session ID が client-initiated bidirectional stream ID に対応しないストリームを受信した場合に H3_ID_ERROR で接続を閉じる MUST を定める。

> Session IDs are derived from the stream ID of the CONNECT stream that established the session and therefore MUST always correspond to a client-initiated bidirectional stream, as defined in Section 2.1 of [RFC9000].
> If an endpoint receives a session ID on a unidirectional stream, bidirectional stream, or datagram that does not correspond to a client-initiated bidirectional stream ID, the endpoint MUST close the connection with an H3_ID_ERROR error code.
> Session IDs that correspond to closed sessions are not considered invalid for the purposes of this check; endpoints handle data for closed sessions as described in Section 6.

現状は不正なストリームを黙って捨てて接続を閉じないため、違反した peer を検知できず、MOQT 層には何も起きていないように見える。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `route_uni_stream` は `classify_uni_stream_checked` の結果を `Err(_) => return` で捨てる。
  依存 `shiguredo_http3` の `StreamHeaderDecodeError` は `InvalidSessionId` を「呼び出し側は H3_ID_ERROR で接続を閉じる」と定義しており、example はこの指示に従っていない。
  `SessionIdOutOfRange` (QUIC のストリーム ID 上限超え) は decode 経路では返らず `StreamHeader::new` からのみ生成されるため、受信経路では到達しない (判定関数の網羅性のために扱いは決めておく)。
- `route_uni_stream` は `ClassifiedUniStream::WebTransport { data_offset, .. }` で `session_id` を捨てる。自セッション以外の session ID を持つ単方向ストリームも `uni_tx` へ渡り、MOQT の data stream として扱われる。
- `route_bi_stream` も `Err(_) => return` でデコードエラーを捨てる。こちらは `StreamHeader::decode_bidirectional_checked` の結果を `session_id` と照合しており、他セッション向けのストリームは HTTP/3 層へ渡している。
- どちらのルーティングタスクも接続を閉じる経路を持たない。タスクは `WtClient::connect` から spawn されるため、`WtSession` のエラー経路にも現れない。

## 設計方針

- ストリームヘッダーのデコードエラーのうち接続エラーに相当するもの (`InvalidSessionId` / `SessionIdOutOfRange`) を接続エラーとして扱い、HTTP/3 の CONNECTION_CLOSE を H3_ID_ERROR で送る。
  h3 層は Sans I/O のため CONNECTION_CLOSE の送出は I/O 層 (s2n-quic) が行う。`s2n_quic::connection::Handle::close` に HTTP/3 のエラーコード (H3_ID_ERROR = 0x108) を application error code として渡す。
- エラーコードの数値は example 側で再定義しない。`shiguredo_http3::ErrorCode::IdError` の `code()` を使う。`StreamHeaderDecodeError` から `ErrorCode` への対応は h3 層が公開していないため、`InvalidSessionId` / `SessionIdOutOfRange` → `ErrorCode::IdError` の対応は example 側の match で書く (数値は書かない)。
- `route_uni_stream` / `route_bi_stream` が接続を閉じられるように、`WtClient::connect` が保持する `s2n_quic::connection::Handle` を clone して渡す。同じ 2 タスクへ接続エラーの共有スロットを足す [issues/0138](../issues/0138-bug-h3-connection-error-propagation.md) とは目的が別だが対象が重なるため、先に実装された側の方式に寄せて共通化する。
- 単方向ストリームでも `ClassifiedUniStream::WebTransport { session_id, .. }` の `session_id` を自セッションの session ID と照合し、他セッション向けのストリームを MOQT 層へ渡さない。双方向ストリームと同じ扱いにそろえる。閉じたセッションの session ID は §4 の末尾により不正ではないため、自セッションでないことだけを理由に接続を閉じない。
  単方向ストリームのルーティングタスクは CONNECT の stream id 確定より前に spawn されるため、自セッションの session ID は `Arc<std::sync::OnceLock<u64>>` などの共有セルで渡し、CONNECT の id 確定後に設定する。未設定の間に届いたストリームは設定されるまで待ってから照合する (closed の [issues/closed/0093](../issues/closed/0093-bug-webtransport-settings-order.md) が記録した「CONNECT より後に spawn する」制約を単方向側では満たせないため)。
- ルーティングの判定 (継続 / 破棄 / `uni_tx` へ / `bi_tx` へ / h3 層へ / 接続クローズ) を純関数に切り出し、spawn されたタスクの外からテストできるようにする。`Ok` の型は方向ごとに異なる (`ClassifiedUniStream` と `(StreamHeader, usize)`) ため、動作を表す自作 enum へ正規化した入力を取る (`fn decide_route(direction: StreamDirection, input: RouteInput) -> RouteAction` など)。
  - `Ok(WebTransport { session_id, .. })`: 自セッションなら単方向は `uni_tx` へ、双方向は `bi_tx` へ。他セッションなら単方向は破棄、双方向は h3 層へ (双方向の現状動作を維持)
  - `Ok(WebTransport 以外)`: 破棄 (WebTransport 以外の単方向ストリーム)
  - `Err(BufferTooShort)`: 追加受信待ちの継続
  - `Err(InvalidSessionId)` / `Err(SessionIdOutOfRange)`: H3_ID_ERROR での接続クローズ
  - `Err(InvalidFormat)`: 破棄 (現状どおり)。単方向では WebTransport 以外の stream type、双方向では signal value が `BIDIRECTIONAL_SIGNAL_VALUE` (0x41) 以外で返る

## 完了条件

- 不正な session ID のストリームを受けたときに接続が H3_ID_ERROR で閉じることを確認できること。確認方法は次のとおり。
  - 単体テスト: 判定関数に `InvalidSessionId` / `SessionIdOutOfRange` を与えると H3_ID_ERROR での接続クローズが選ばれ、`BufferTooShort` は継続、`InvalidFormat` は破棄になること (variant は直接構築して渡す。`SessionIdOutOfRange` は受信経路では到達しないが、判定関数の網羅性を固定する)。他セッションの session ID を持つ単方向ストリームの `Ok` が `uni_tx` へ渡らない action になること
  - 実機: `session_id % 4 != 0` の WebTransport 単方向 / 双方向ストリームを送るテストクライアントを用意し、example 側の接続が閉じることを `RUST_LOG=debug` のログで確認する。WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う
- 他セッション向けの単方向ストリームが MOQT 層 (`uni_tx`) へ渡らないこと (単体テストまたは再現手順で確認)
- 正常な session ID のストリームの扱いが変わらないこと (既存のルーティングが通ること)

## 解決方法

draft-ietf-webtrans-http3-16 §4 (WebTransport Features) の MUST に従い、client-initiated bidirectional stream ID に
対応しない session ID を受けたときに H3_ID_ERROR で接続を閉じるようにした。

- `examples/moqt-transport/src/webtransport.rs`
  - ルーティングの判定を純関数 `decide_route(StreamDirection, RouteInput) -> RouteAction` に切り出した。
    `RouteInput` は `WebTransport { own_session }` / `Http3` / `BufferTooShort` / `InvalidSessionId` /
    `SessionIdOutOfRange` / `InvalidFormat`、`RouteAction` は `ForwardToMoqt` / `ForwardToH3` / `Discard` /
    `Continue` / `CloseConnection(ErrorCode)` である
  - `InvalidSessionId` / `SessionIdOutOfRange` は `shiguredo_http3::ErrorCode::IdError` (0x108。
    数値は example 側で再定義しない) を `s2n_quic` の application error code として `Handle::close` に渡す。
    h3 層は Sans I/O のため CONNECTION_CLOSE の送出は I/O 層が行う
  - `route_uni_stream` / `route_bi_stream` を判定駆動に書き換えた。単方向は WebTransport 以外を HTTP/3 層へ流し、
    他セッションの WebTransport ストリームを MOQT 層へ渡さず FIN まで読み捨てる (双方向は従来どおり HTTP/3 層へ)
  - 単方向ストリームのルーティングタスクは CONNECT より前に spawn されるため、session ID を `watch` で共有し、
    確定するまで照合を待つ (`wait_for_session_id`)。確定前に届いたストリームを他セッション扱いしない
  - 追加したテスト: `InvalidSessionId` / `SessionIdOutOfRange` が両方向で H3_ID_ERROR の接続クローズになること
    (0x108 の数値も固定)、`BufferTooShort` は継続、`InvalidFormat` は読み捨て、`Http3` は HTTP/3 層へ、
    自セッションは MOQT 層へ、他セッションの単方向は MOQT 層へ渡さず読み捨て・双方向は HTTP/3 層へ
- `CHANGES.md` の `## develop` に `[FIX]` を追加した (datagram 経路が対象外である理由も併記)

## 対象外 (別 issue 候補)

- datagram 経路の §4 対応: 依存する `shiguredo_http3` が datagram の session ID 不正に H3_DATAGRAM_ERROR を
  返すため、H3_ID_ERROR にするには crate 側の変更が要る
- 実機確認: WebTransport セッションの確立自体が [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md)
  の解消待ちであるため、実 peer での確認は行っていない (単体テストで判定を固定)
