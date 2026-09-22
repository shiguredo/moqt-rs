# ALPN で transport を選択できるようにする

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/add-alpn-transport-selection
- Polished: 2026-09-22

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
  datagram endpoint はハンドシェイク前の builder にしか差し込めないため、`quic::connect` の中で `Client::builder().with_datagram(...)` として設定している。
- `examples/moqt-transport/src/webtransport.rs` の `H3_ALPN` は `h3` のみである。証明書検証を行う経路は `s2n_quic::Client::builder().with_tls(ca_pem.as_str())` で ALPN を明示せず、s2n-quic-rustls 0.88.0 の既定 (`h3`) に依存している。
  同 file の `WtClient::connect` は QUIC 接続の確立と WebTransport セッションの確立を 1 関数で行い、`h3_settings` を h3 状態へ渡し、`set_webtransport_transport_verified(true, false)` を CONNECT 前に実行する。
- `examples/moqt-publisher/src/pipeline.rs` と `examples/moqt-subscriber/src/pipeline.rs` の `run` は `match config.url.transport` で経路を固定し、接続後にサーバーが選んだ ALPN を見ない。
- 依存 s2n-quic の `s2n_quic::connection::Connection` / `s2n_quic::connection::Handle` には `application_protocol()` があり、ネゴシエートされた ALPN を `Result<bytes::Bytes>` で取得できる (未選択なら空、取得失敗は `Err`)。example はこれを使っていない。

## 設計方針

- scheme による指定を優先しつつ、ALPN の提示と選択結果で経路を決める。`moqt://` は QUIC 接続を維持したまま、サーバーが `h3` を選んだ場合だけ WebTransport 経路へ進む。
  - `moqt://`: ClientHello で `moqt-21` を先、`h3` を後に提示する。接続後に `application_protocol()` を読み、`moqt-21` なら `MoqtClient::establish_quic` (draft-21 §6.2.2 の経路)、`h3` なら WebTransport の CONNECT (§6.2.1 の経路) へ進む。
  - `https://`: 現状どおり `h3` のみを提示し WebTransport へ進む。
- QUIC 接続の確立を 1 箇所にまとめる。builder で決める値はハンドシェイク前に確定する必要があるため、scheme ごとに提示 ALPN を決めて builder へ渡し、datagram endpoint はどちらの scheme でも常に設定する。ハンドシェイク後に ALPN で分岐する。
  - 提示 ALPN: `moqt://` は `[moqt-21, h3]` の順 (`moqt-21` を先)、`https://` は `[h3]` のみ (現状どおり)。この違いが `https://` の既存挙動を変えない条件になる。
  - datagram endpoint は WebTransport 経路に限らず常に設定する (draft-21 §6.2 が "The QUIC DATAGRAM extension ([RFC9221]) MUST be supported and negotiated in the QUIC connection used for MOQT" と定め、QUIC 経路でも必須である)。builder は接続確立前にしか作れないため条件付きにはできない。
  - s2n-quic の builder に h3 の transport parameter を設定する口は無い。h3 の状態 (`h3_settings`、`set_webtransport_transport_verified`、CONNECT) は ALPN が `h3` と確定した後だけ用意する。
- `WtClient::connect` を「QUIC 接続の確立」と「確立済み接続上での WebTransport セッション確立」に分ける。`moqt://` 経路で確立した接続をそのまま WebTransport の確立へ渡せるようにする。分岐後の WebTransport 経路では publisher / subscriber の pipeline が `enable_webtransport` と datagram 受信 (`receive_datagrams`) を設定する (現行の `Transport::WebTransport` 分岐と同じ)。
- 選択された ALPN から経路を決める純関数を `examples/moqt-transport/src/lib.rs` に置き、publisher / subscriber の pipeline で共有する。入力は `application_protocol()` を処理した後の `Option<&[u8]>` (空は `None` として扱う) とし、戻り値は新しい enum (`NegotiatedPath::{QuicNative, WebTransport}` など) にする。
  scheme 由来の `Transport` とは別に、進む経路を表す型にする (`moqt://` から WebTransport へ入る場合に scheme の区別を落とさない)。判定不能な ALPN と `Err` は接続を閉じてエラーにする。
- 証明書検証を行う経路でも ALPN を明示する。`s2n_quic::Client::builder().with_tls(ca_pem.as_str())` には ALPN を設定する口が無いため、`s2n_quic_rustls::Client::builder().with_certificate(path).with_application_protocols(...)` へ切り替える (証明書検証をスキップする経路の `build_insecure_tls_client` と同じ ALPN を渡す)。
- 選択結果と進んだ経路をログに出す。どちらの経路に入ったかを実機で判別できるようにする。
- `moqt://` でサーバーが `moqt-21` を選ぶ場合の挙動と、`https://` の挙動は変えない。`examples/README.md` の URL スキーム節は「`moqt://` は QUIC を優先し、サーバーが `h3` を選べば WebTransport へフォールバックする」と実装に合わせて更新する。
- WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであり、`moqt://` から WebTransport へ入る経路も同じ制約を受ける (relay が `wt_enabled` を広告する現行構成では CONNECT が拒否される)。
- 実装順は 0132 → 0133 → 0135 → 0136 → 0137 → 0138 → 0139 とする。`examples/moqt-transport/src/lib.rs` を 0132 / 0133 が、`webtransport.rs` の `WtClient::connect` を 0135〜0138 が変更するため、それらが入った後に着手する。

## 完了条件

- サーバーが `h3` を選んだときに WebTransport 経路へ進めることを確認できること。確認方法は次のとおり。
  - 単体テスト: 選択された ALPN のバイト列から経路を決める関数で、`moqt-21` → QUIC 経路、`h3` → WebTransport 経路、`None` (未選択 / 空) と未知の値 → エラーになること。判定関数の入力は `Option<&[u8]>` とし、`application_protocol()` の `Err` は判定前に接続エラーとして扱う
  - 実機 (ALPN のログ): relay を `h3` を選ぶ構成 (別途用意する relay の設定) と `moqt-21` を選ぶ構成で起動し、`moqt-publisher --url moqt://<relay>` / `moqt-subscriber --url moqt://<relay>` の ALPN ログが選択結果と一致することを `RUST_LOG=debug` で確認する
  - 実機 (WebTransport の成立): WebTransport の CONNECT が成立して MOQT セッションが確立することを確認する。WebTransport セッションの確立自体は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであるため、この確認は draft-15 相当の peer または 0094 の解消後に行う
- relay が `moqt-21` を選ぶ構成で `moqt://` の既存挙動 (QUIC 直接接続) が変わらないこと
- `https://` の既存挙動 (WebTransport) が変わらないこと
- 証明書検証を行う経路でも ALPN が明示され、s2n-quic-rustls の既定値に依存しないこと
- `examples/README.md` の URL スキーム節が実装に合わせて更新されていること
