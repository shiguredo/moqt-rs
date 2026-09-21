# WT-Protocol を検証して WT_ALPN_ERROR を送る

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-protocol-header
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 §3.3 (Application Protocol Negotiation) は、アプリケーションプロトコル交渉を要求するクライアントが、成功応答に `WT-Protocol` が無い / 不正 / 申告していない値だった場合に `WT_ALPN_ERROR` でセッションを閉じる MUST を定める。

> A client that requires application protocol negotiation MUST close the WebTransport session with a WT_ALPN_ERROR error code if the server does not include a WT-Protocol header field, or if it is malformed and therefore ignored, in a successful response.

> If the client sends a WT-Available-Protocols header field and the server responds with a WT-Protocol header field, the value in the WT-Protocol response header field MUST be one of the values listed in WT-Available-Protocols of the request.
> If the client receives a WT-Protocol value that was not included in its WT-Available-Protocols list, the client MUST close the WebTransport session with a WT_ALPN_ERROR error code.

draft-ietf-moq-transport-21 §6.2 (Session establishment) は、この仕組みを MOQT のバージョン交渉に使うと定める。

> MOQT uses ALPN in QUIC and "WT-Available-Protocols" in WebTransport ([WebTransport], Section 3.3) to perform version negotiation.

現状は 2xx だけで確立とみなすため、サーバーがプロトコルを選ばなかった / 別のプロトコルを選んだ場合もバージョン不一致のまま MOQT を続行してしまう。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `WtClient::connect` は `ConnectRequest::new("https", &config.authority, path).available_protocols(vec![crate::MOQT_PROTOCOL.to_string()])` を送る。`WT-Available-Protocols` には `moqt-21` だけを列挙している。
- 同 `connect_outcome` は `:status` だけを蓄積し、`Event::HeadersEnd` で 2xx かどうかを判定する。`WT-Protocol` は見ないため、応答に `WT-Protocol` が無い場合も申告外の値の場合も `ConnectOutcome::Established` になる。
- `examples/moqt-transport/src/error.rs` の `TransportError` にプロトコル交渉失敗を表す variant が無く、呼び出し元 (publisher / subscriber の pipeline) は失敗理由を `ConnectFailed { status }` と区別できない。
- セッション確立後は datagram 以外の h3 イベントを捨てるため、h3 層が `WebTransportEvent::SessionClosed { error_code, .. }` を発火しても example は気づかない。
- 依存 `shiguredo_http3` には `ConnectResponse::parse_protocol` (Structured Fields Item の String のみを受理) と `ConnectResponse::is_protocol_valid(&ConnectRequest)` があり、§3.3 の判定 (交渉を要求して `WT-Protocol` 無しは無効、申告外の値は無効、交渉不要なら有効) を実装済みである。example はこれを使っていない。

## 設計方針

- `connect_outcome` で `wt-protocol` ヘッダーも収集し、`Event::HeadersEnd` の時点で `ConnectResponse::is_protocol_valid` を使って 2xx とプロトコル選択の両方を検証する。`ConnectResponse::parse_protocol` を使い、ヘッダー値のパースを example 側で再実装しない。
- 無効時は §3.3 の MUST に従い `WT_ALPN_ERROR` (0x0817b3dd) で接続を閉じる。確立前なので `s2n_quic::connection::Handle::close` に `WT_ALPN_ERROR` を application error code として渡す。h3 層は Sans I/O であり CONNECTION_CLOSE の送出 API を持たないため、I/O 層 (s2n-quic) で行う。
- `TransportError` にプロトコル交渉失敗の variant を追加し、閉じた理由 (応答に `WT-Protocol` が無い / 申告外の値 / パース不能) を呼び出し元が判別できるようにする。`ConnectFailed { status }` と混ぜない。
- h3 層のセッション終了イベント (`WebTransportEvent::SessionClosed` など) も connect の判定に反映し、`WT_ALPN_ERROR` で閉じた場合は MOQT セッションを確立しない。datagram 以外のイベントを捨てている現状の経路をやめる。
- サーバーが `WT-Protocol` を返さない構成を許すかどうかは §3.3 の MUST に従い許さない。example は常に `WT-Available-Protocols` を送るため、交渉は必須として扱う。

## 完了条件

- `WT-Protocol` が無い / 申告外の値のときにセッションを確立しないことを確認できること。確認方法は次のとおり。
  - 単体テスト: 応答判定 (`connect_outcome` または切り出した判定関数) に `:status: 200` と `wt-protocol` 無し / `moqt-20` のような申告外の値の Header イベント列を与え、確立せず `WT_ALPN_ERROR` の失敗になること。`moqt-21` を返す場合は確立になること。壊れた Structured Fields 値 (`wt-protocol` が String 以外) も失敗になること
  - 実機: relay 側で `WT-Protocol` を返さない構成と、申告外の値を返す構成を用意し、example が MOQT の SETUP を送らずに `WT_ALPN_ERROR` で接続を閉じることを `RUST_LOG=debug` のログで確認する。
    WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う
- 正しい `WT-Protocol` (`moqt-21`) を返すサーバーでは従来どおり MOQT セッションが確立すること
