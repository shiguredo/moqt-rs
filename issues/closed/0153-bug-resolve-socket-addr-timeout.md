# 接続先の名前解決にタイムアウトが無い

- Created: 2026-09-24
- Completed: 2026-09-29
- Branch: feature/fix-resolve-socket-addr-timeout
- Polished: 2026-09-27

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

## 解決方法

`examples/tokio-moq/src/lib.rs` の名前解決にタイムアウトを追加した。

- `RESOLVE_TIMEOUT` を `Duration::from_secs(5)` で定義し、`resolve_socket_addr` の名前解決を `tokio::time::timeout` で包んだ。打ち切った場合は `TransportError::ResolutionFailed` に `name resolution timed out after 5s` を含めて返す
- タイムアウト値を引数に取る private な `resolve_socket_addr_within` に解決処理を移し、公開 API の `resolve_socket_addr` は `RESOLVE_TIMEOUT` を渡すだけにした。応答しないリゾルバを用意できない環境でも打ち切りの経路を単体テストで固定するためである
- IP リテラルの経路は `SocketAddr` への直接パースのまま (OS のリゾルバを通らないためタイムアウトの影響を受けない)。解決は接続前に 1 回だけで、リトライと再解決は行わない

追加・更新したテスト (`examples/tokio-moq/src/lib.rs` の `tests` モジュール):

- `resolve_socket_addr_times_out_when_resolver_does_not_answer`: `.invalid` (RFC 2606 §2) の名前を 100 ナノ秒のタイムアウトで解決し、`ResolutionFailed` と `name resolution timed out` / タイムアウト値 / 入力 authority がメッセージに含まれ、`QUIC:` が付かないことを固定する
- `resolve_socket_addr_reports_resolution_failure`: タイムアウトを超えなければ従来どおり名前解決の失敗がそのまま伝わることを固定する
- `resolve_socket_addr_succeeds_within_timeout`: `127.0.0.1` と `localhost` の解決がタイムアウトを包んだ後も成功することを固定する

実機確認 (`RESOLVE_TIMEOUT` を一時的に `Duration::from_nanos(1)` にして実行し、確認後に `Duration::from_secs(5)` へ戻した):

- `./target/debug/moq-pub --url moqt://example.invalid:4433/app` が `Fatal: failed to resolve 'example.invalid:4433': name resolution timed out after 1ns` で exit 1
- `./target/debug/moq-sub --url moqt://example.invalid:4433/app` も同じメッセージで exit 1
- タイムアウト値 100 ミリ秒では `getaddrinfo` の失敗 (約 80〜100 ミリ秒) が先に返る場合があるため、打ち切りの確認には 1 ナノ秒を使った

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
