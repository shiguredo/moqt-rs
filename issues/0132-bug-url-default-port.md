# ポート省略 URL で既定ポート 443 を使う

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-url-default-port
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.1.2 (Dereferencing a MOQT URI) は、URI でポートが省略された場合に既定ポート 443 を使うと規定する。

> If the port is omitted in the URI, a default port of 443 is used.

現状はポートを省略した URL を解釈できず、接続を試みる前に必ず失敗する。`moqt://relay.example.com/app` のような URL で publisher / subscriber が接続できるようにする。

## 現状

- `examples/moqt-transport/src/lib.rs` の `parse_url` は scheme を取り除いた残りを `split_authority_path` に渡すだけで、authority にポートがあるかを見ない。`ServerUrl::authority` には URL の authority がそのまま入る。
- `examples/moqt-transport/src/quic.rs` の `connect` は `authority.parse::<std::net::SocketAddr>()` で接続先を作る。ポートが無い authority はパースに失敗し、`TransportError::Quic` の "invalid server address" になる。
- `examples/moqt-publisher/src/pipeline.rs` の `run` (`Transport::WebTransport` 分岐) と `examples/moqt-subscriber/src/pipeline.rs` の `run` (`Transport::WebTransport` 分岐) も同じく `config.url.authority.parse::<std::net::SocketAddr>()` を使い、"invalid server address" で失敗する。
- ポートを補うだけではホスト名の authority は解決できない。`std::net::SocketAddr` は IP アドレスしか表せないため `relay.example.com:443` もパースに失敗する。example には DNS 解決の経路が無く、`examples/README.md` の例は `moqt://127.0.0.1:4443` のように IP リテラルとポートを明示する形になっている。

## 設計方針

- `examples/moqt-transport/src/lib.rs` に authority から host と port を取り出す関数を追加する。`host_from_authority` と同じ IPv6 リテラルの扱い (`[` から `]` までを host とする) を共有し、ポートが無い場合は 443 を既定値にする。
- 接続先 `SocketAddr` の決定を 1 箇所に集約する。host が IP リテラルなら `SocketAddr` に直接パースし、ホスト名なら `tokio::net::lookup_host` で解決する。`quic::connect` と publisher / subscriber の pipeline はこの関数の結果を使い、`authority.parse::<SocketAddr>()` の重複を残さない。`quic::connect` は authority ではなく解決済みの `SocketAddr` を受け取る形にする。
- `ServerUrl::authority` は URL の authority をそのまま保持する。SETUP の AUTHORITY option (`build_setup_options`) と WebTransport CONNECT の `:authority` (`ClientConfig::authority`) には元の URL の authority を渡し続け、省略されたポートを勝手に補わない。
- ポートを明示した URL の既存挙動は変えない。

## 完了条件

- ポート省略 URL の authority から接続先アドレスを組み立てる関数の単体テストが追加され、次が固定されていること
  - `moqt://127.0.0.1/app` の authority から `127.0.0.1:443` が得られる
  - `https://[2001:db8::1]/app` の authority から `[2001:db8::1]:443` が得られる
  - ポート付きの authority (`127.0.0.1:4443`) はそのまま使われる
  - ホスト名の authority は解決結果に 443 が使われる (テストは解決関数へ `SocketAddr` を渡す形にし、DNS に依存させない)
- publisher / subscriber の両経路で、ポート省略 URL からの接続が成立すること。確認手順を次に示す。
  1. relay を 443 で待ち受ける (443 を待ち受けられない環境では、テスト用に relay の待ち受けポートを変えて同じ手順を行う)
  2. `moqt-publisher --url moqt://127.0.0.1/app` を起動し、接続先が `127.0.0.1:443` になることと SETUP が成立することをログで確認する
  3. `moqt-subscriber --url moqt://127.0.0.1/app` でも同じ確認を行う
- `Transport::WebTransport` 分岐も同じアドレス決定関数を通ること。WebTransport 経路の実接続確認は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) (RESET_STREAM_AT 非対応) の解消後に relay の応答次第で行う。
