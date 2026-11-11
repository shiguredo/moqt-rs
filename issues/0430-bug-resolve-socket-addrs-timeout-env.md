# resolve_socket_addrs のタイムアウトテストが .invalid が即時失敗する環境で必ず失敗する

- Created: 2026-11-11
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-resolve-socket-addrs-timeout-env
- Polished: {YYYY-MM-DD}

## 目的

`crates/tokio-moq/src/lib.rs` の `resolve_socket_addrs_times_out_when_resolver_does_not_answer` は `.invalid` への名前解決がタイムアウトに打ち切られることを検証するが、`.invalid` が即時の否定応答 (`EAI_NONAME`) で返る環境ではタイムアウト経路が勝たず、`err.to_string()` に `name resolution timed out` が現れず必ず失敗する。
タイムアウト経路を resolver の挙動に依存せず決定的に検証できるようにする。

## 現状

- 2026-11-11 の調査環境 (ローカル開発環境) では 5/5 実行すべて失敗。panic: `打ち切りが理由として分かること: failed to resolve 'example-<pid>.invalid': failed to lookup address information: Name or service not known`。
- テストは `tokio::time::timeout(100ns, tokio::net::lookup_host("example-<pid>.invalid"))` で、pid 接尾辞による名前の一意化 (2826 行付近) を行っているが、`.invalid` が即時失敗する環境では 100ns より速く `EAI_NONAME` で future が完成する。
- タイムアウト経路の検証を本物の DNS に依存させているのが本質であり、resolver の挙動はテストから統制できない。
- `resolve_socket_addrs_within` (916-944 行付近) は `authority_parts` → `socket_addr_string` → IP リテラル fast path → `timeout(lookup_host)` → `resolve_failed` 整形の構造。差し替えるべきは `tokio::net::lookup_host(addr_text)` の呼び出しのみ。
- 前例: `connect_with_fallback` は同型の resolver 注入 API (959-964 行付近) で、テストが `Err` を返すクロージャを注入している (2959-2965 行付近)。

## 設計方針

- `resolve_socket_addrs` を future 注入可能な core 関数に分割する。`resolve_socket_addrs_with(authority, timeout, lookup)` を新設し (`lookup: FnOnce(String)
   -> impl Future<Output = std::io::Result<impl Iterator<Item = SocketAddr>>>` 相当)、`resolve_socket_addrs_within` は実 DNS (`tokio::net::lookup_host`) を注入する薄いラッパーのままシグネチャを変えない。
  既存の呼び出し点は 1 行も変わらない。
- タイムアウトテストは hanging future (`std::future::pending::<std::io::Result<std::vec::IntoIter<SocketAddr>>>()`) を注入してタイムアウト経路を決定的に検証する。実 DNS は `resolve_socket_addrs_reports_resolution_failure` が引き続き担う。
- hanging future はモックやスタブではなく「応答しない resolver」というテストの前提をそのまま表す fixture であり、応答を偽装しない (`connect_with_fallback` の注入クロージャと同領域の前例)。
- `start_paused = true` は使わない (実 DNS を使うテスト `resolve_socket_addrs_reports_resolution_failure` が spawn_blocking 待ちを runtime の auto-advance で即時打ち切る誤動作を避ける)。hanging future なら実時間の `#[tokio::test]` で経路は決定的である。
- タイムアウトテストの doc コメントの「一意名 + キャッシュ回避」の説明を hanging future 方式に合わせて書き換える。

## 完了条件

- タイムアウトテストが resolver の挙動 (`.invalid` の即時失敗を含む) に依存せず、全環境で決定的に通ること。タイムアウト理由が `name resolution timed out` として現れ、authority を含み、`QUIC` を含まないこと。
- `resolve_socket_addrs_within` のシグネチャと既存呼び出し点が変わらないこと。実 DNS を使う既存テスト (IP リテラル / 失敗整形 / 経路の他テスト) が壊れないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
