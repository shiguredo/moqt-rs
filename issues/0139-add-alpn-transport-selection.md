# ALPN で transport を選択できるようにする

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/add-alpn-transport-selection
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.1.2 (Dereferencing a MOQT URI) は、クライアントが QUIC の ClientHello で MOQT の ALPN と `h3` を優先順に提示し、サーバーが選んだ ALPN に応じて経路を進める手順を示す。

> The client MAY use either native QUIC or WebTransport.  On a QUIC
> connection, the client offers any combination of MOQT ALPNs (e.g.
> moqt-1, moqt-2) and h3 that it supports in its TLS ClientHello, in
> preference order.  If the server selects an MOQT ALPN, the session
> proceeds as described in Section 6.2.2.  If the server selects h3, the
> client establishes a WebTransport session as described in Section
> 6.2.1.  On a TCP+TLS connection, the client offers h2 in its TLS
> ClientHello and establishes a WebTransport session as described in
> Section 6.2.1.

§6.2 はこの ALPN と `WT-Available-Protocols` をバージョン交渉に使うと定める。

> MOQT uses ALPN in QUIC and "WT-Available-Protocols" in WebTransport ([WebTransport], Section 3.3) to perform version negotiation.

現状は URL スキームで transport を固定するため、`moqt://` で接続したサーバーが `h3` を選んでも WebTransport 経路へフォールバックできない。

## 現状

- `examples/moqt-transport/src/lib.rs` の `parse_url` は scheme だけで `Transport::Quic` / `Transport::WebTransport` を決める。`Transport` は接続前に確定する。
- `examples/moqt-transport/src/quic.rs` の `ALPN` は `crate::MOQT_PROTOCOL` (`moqt-21`) のみで、`build_tls_client` は `with_application_protocols([ALPN].iter())`、証明書検証をスキップする経路は `build_insecure_tls_client(&[ALPN])` を使う。`h3` は提示しない。
- `examples/moqt-transport/src/webtransport.rs` の `H3_ALPN` は `h3` のみである。証明書検証を行う経路は `s2n_quic::Client::builder().with_tls(ca_pem.as_str())` で ALPN を明示せず、s2n-quic-rustls 0.88.0 の既定 (`h3`) に依存している。
- `examples/moqt-publisher/src/pipeline.rs` と `examples/moqt-subscriber/src/pipeline.rs` の `run` は `match config.url.transport` で経路を固定し、接続後にサーバーが選んだ ALPN を見ない。
- 依存 s2n-quic の `s2n_quic::connection::Connection` / `s2n_quic::connection::Handle` には `application_protocol()` があり、ネゴシエートされた ALPN を `bytes::Bytes` で取得できる。example はこれを使っていない。

## 設計方針

- scheme による指定を優先しつつ、ALPN の提示と選択結果で分岐する。
  - `moqt://`: ClientHello で `moqt-21` を先、`h3` を後に提示する。接続後に `application_protocol()` を読み、`moqt-21` なら `MoqtClient::establish_quic` (draft-21 §6.2.2 の経路)、`h3` なら WebTransport の CONNECT (§6.2.1 の経路) へ進む。
  - `https://`: 現状どおり `h3` のみを提示し WebTransport へ進む。証明書検証を行う経路も ALPN を明示して既定値への依存をやめる。
- `WtClient::connect` を「QUIC 接続の確立」と「確立済み接続上での WebTransport セッション確立」に分ける。`moqt://` 経路で確立した `s2n_quic::Connection` をそのまま WebTransport の確立へ渡せるようにする。datagram endpoint と h3 設定は WebTransport 経路のときだけ用意する。
- 選択された ALPN から `Transport` (または次の経路) を決める純関数を `examples/moqt-transport/src/lib.rs` に置き、publisher / subscriber の pipeline で共有する。判定不能な ALPN は接続を閉じてエラーにする。
- 選択結果と進んだ経路をログに出す。どちらの経路に入ったかを実機で判別できるようにする。
- 既存の `moqt://` (サーバーが `moqt-21` を選ぶ) と `https://` の挙動は変えない。

## 完了条件

- サーバーが `h3` を選んだときに WebTransport 経路へ進めることを確認できること。確認方法は次のとおり。
  - 単体テスト: 選択された ALPN のバイト列から経路を決める関数で、`moqt-21` → QUIC 経路、`h3` → WebTransport 経路、未知の値 / 空 → エラーになること
  - 実機: relay が `h3` を選ぶ構成で `moqt-publisher --url moqt://<relay>` / `moqt-subscriber --url moqt://<relay>` を起動し、
    WebTransport の CONNECT が成立して MOQT セッションが確立することを `RUST_LOG=debug` の ALPN ログで確認する。
    WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md)
    の解消待ちであるため、実機確認は draft-15 相当の peer または 0094 の解消後に行う
- relay が `moqt-21` を選ぶ構成で `moqt://` の既存挙動 (QUIC 直接接続) が変わらないこと
- `https://` の既存挙動 (WebTransport) が変わらないこと
