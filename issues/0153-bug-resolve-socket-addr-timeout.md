# 接続先の名前解決にタイムアウトが無い

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-resolve-socket-addr-timeout
- Polished: {YYYY-MM-DD}

## 目的

名前解決が応答しない環境で publisher / subscriber の起動が止まらないようにする。`tokio::net::lookup_host` は OS の getaddrinfo に依存し、応答が無い場合は数十秒から数分ブロックする。

## 現状

`examples/moqt-transport/src/lib.rs` の `resolve_socket_addr` は `tokio::net::lookup_host` をタイムアウト無しで待つ。

`examples/moqt-publisher/src/pipeline.rs` と `examples/moqt-subscriber/src/pipeline.rs` は transport 分岐の前にこの関数を呼ぶ。解決が返らないとセッションを作る前に止まるため、セッションのタイムアウト (30 秒) も効かずログも出ない。

## 設計方針

- `resolve_socket_addr` の名前解決を `tokio::time::timeout` で包み、既定の解決タイムアウトを定数 (`RESOLVE_TIMEOUT`) として定義する
- タイムアウトした場合は `TransportError::ResolutionFailed` を返す。利用者向けの表示に `QUIC:` を付けない方針を維持する
- 解決は接続前に 1 回だけ行う現状の構造を変えない。リトライや再解決は行わない
- IP リテラルの経路は OS のリゾルバを通らないためタイムアウトの影響を受けない

## 完了条件

- 名前解決に `tokio::time::timeout` が使われ、超えた場合に `TransportError::ResolutionFailed` になること
- IP リテラル (`127.0.0.1` / `[::1]`) と `localhost` の解決が従来どおり成功すること
- タイムアウトの行使を実機で確認すること。応答しないリゾルバを用意できない環境では、タイムアウトを一時的に極端に短い値にして `ResolutionFailed` になることを確認し、その確認方法を解決方法に記録する
