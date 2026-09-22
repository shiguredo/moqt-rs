# WT-Protocol を検証して WT_ALPN_ERROR を送る

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-webtransport-protocol-header
- Polished: 2026-09-22

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
- 依存 `shiguredo_http3` の h3 層は §3.3 の判定を既に実装している。`handle_wt_connect_response` が 2xx 応答の `wt-protocol` を `ConnectResponse::parse_protocol` で解析し、交渉を要求していて無い / 申告外なら `terminate_wt_session_with(stream_id, WtErrorCode::AlpnError as u64, ...)` を呼ぶ。
  判定は h3 内に実装されており、`ConnectResponse::is_protocol_valid` は h3 のテストからのみ呼ばれる。
  このとき `WebTransportEvent::SessionClosed { error_code: 0x0817b3dd, .. }` が発火し、`SessionEstablished` は発火しない。example はこのイベントを捨てているため判定結果が届かない。
- `examples/moqt-transport/src/error.rs` の `TransportError` にプロトコル交渉失敗を表す variant が無く、呼び出し元 (publisher / subscriber の pipeline) は失敗理由を `ConnectFailed { status }` と区別できない。
- セッション確立後は datagram 以外の h3 イベントを捨てるため、h3 層が `WebTransportEvent::SessionClosed { error_code, .. }` を発火しても example は気づかない。この経路の書き換えは [issues/0136](../issues/0136-bug-webtransport-session-termination.md) が担当する。
- 依存 `shiguredo_http3::webtransport::WtErrorCode::AlpnError` (`= 0x0817b3dd`) が公開されているため、example 側で数値を再定義する必要はない (h3 層も `WtErrorCode::AlpnError as u64` で同じ値を使う)。

## 設計方針

- §3.3 の判定を example で再実装しない。判定は h3 層が `handle_wt_connect_response` で行い、結果を `WebTransportEvent` で通知する。example は `connect_outcome` で h3 のイベントを観測して結果を受け取る。
  h3 は交渉失敗時に `Event::Header` / `Event::HeadersEnd` を push せずに `SessionClosed` を発火するため、`HeadersEnd` を待たずに失敗と判定できるようにする。
- `connect_outcome` は `Event::HeadersEnd` で早期 return せず、`:status` とヘッダー終端を蓄積して `SessionEstablished` / `SessionClosed` を観測してから判定する (h3 は交渉失敗時に `HeadersEnd` を push せず `SessionClosed` を届けるため、`HeadersEnd` で確定させると `SessionEstablished` を取りこぼす)。判定は次のとおりにする。
  - `WebTransportEvent::SessionEstablished`: 確立 (2xx の `:status` を確認済みであることを条件にする)
  - `WebTransportEvent::SessionClosed { error_code, .. }` が確立前に届いたら失敗。`error_code == WtErrorCode::AlpnError as u64` ならプロトコル交渉失敗、それ以外は接続エラーとして扱う
  - `Event::HeadersEnd` までに 2xx の `:status` が無い: 従来どおり `ConnectFailed { status }`
- プロトコル交渉失敗時は §3.3 の MUST に従い `WT_ALPN_ERROR` で接続を閉じる。h3 層は Sans I/O であり CONNECTION_CLOSE の送出 API を持たないため、`s2n_quic::connection::Handle::close` に `s2n_quic::application::Error::new(WtErrorCode::AlpnError as u64)` を渡す (既存の `WtSendStream::reset` と同じく `application::Error` 経由)。数値は example 側に直書きしない。
- `TransportError` に `ProtocolNegotiationFailed { error_code: u64 }` を追加し、`ConnectFailed { status }` と区別する。h3 層の `SessionClosed` は失敗理由 (無し / 申告外 / パース不能) を区別せず `error_code` だけを運び、`close_message` は空文字列のため、3 理由は example 側で切り分けない (`Display` の実装も error_code を含む文言にする)。
- 確立後のセッション終了イベントの処理は [issues/0136](../issues/0136-bug-webtransport-session-termination.md) の担当とする。0137 は「0136 が整える経路で `SessionClosed { error_code: AlpnError }` を観測したら MOQT セッションを確立しない」ところまでを担い、datagram 以外のイベントを捨てる現状の経路の書き換え自体は行わない。実装順は 0136 → 0137 → 0138 とし、先に実装された側の方式に寄せて共通化する。
- サーバーが `WT-Protocol` を返さない構成を許すかどうかは §3.3 の MUST に従い許さない。example は常に `WT-Available-Protocols` を送るため、交渉は必須として扱う。

## 完了条件

- `WT-Protocol` が無い / 申告外の値のときにセッションを確立しないことを確認できること。確認方法は次のとおり。
  - 単体テスト: 応答判定 (切り出した判定関数) に次のイベント列を与えて固定する (`examples/moqt-transport/src/webtransport.rs` の `#[cfg(test)] mod tests`。h3 の `Event` は公開 variant / 公開フィールドなので構成できる)
    - `:status: 200` → `HeadersEnd` → `SessionEstablished` で確立
    - `SessionClosed { error_code: WtErrorCode::AlpnError as u64, .. }` (h3 は交渉失敗時に `HeadersEnd` を push しない) で、プロトコル交渉失敗の variant になる
    - 確立前に別の `error_code` を持つ `SessionClosed` が届いた場合も失敗になる (プロトコル交渉失敗とは別の扱いになることを固定する)
    - `:status: 500` → `HeadersEnd` で `ConnectFailed { status: Some(500) }` のまま
    - `:status` が届かず `HeadersEnd` で `ConnectFailed { status: None }` のまま
  - 実機: relay 側で `WT-Protocol` を返さない構成と、申告外の値を返す構成を用意し、example が MOQT の SETUP を送らずに `WT_ALPN_ERROR` で接続を閉じることを `RUST_LOG=debug` のログで確認する。
    WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う
- 正しい `WT-Protocol` (`moqt-21`) を返すサーバーでは従来どおり MOQT セッションが確立すること
- プロトコル交渉失敗の variant が `ConnectFailed { status }` と区別でき、`WT_ALPN_ERROR` で接続を閉じる経路が example 側の数値直書きを含まないこと (`WtErrorCode::AlpnError as u64` を `s2n_quic::application::Error::new` に渡す)
