# SETUP の AUTHORITY に含まれる IPv6 リテラルを RFC 3986 の IPv6address として検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-authority-ipv6-validation
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §9.1.1 (AUTHORITY) は AUTHORITY が RFC 3986 の URI formatting rules に従うことを要求し、"If an AUTHORITY option does not conform to these rules, the session MUST be closed with MALFORMED_AUTHORITY." と MUST を課す。現状の IPv6 リテラル検証は RFC 3986 §3.2.2 の `IPv6address`
より緩く、かつ IPv4-mapped 表記の正当な値と IPv4 末尾が `::` の直後に来る正当な値を拒否するため、この MUST を満たしていない。

## 現状

- `src/parameter.rs` の `validate_ipv6addr` は `split(':')` の空セグメントを `filter` で捨て、`matches("::")` の回数を数えるだけで検証する。このため `[1:::2]`、`[1:2:3:4:5:6:7:8:]`、`[:1:2:3:4:5:6:7:8]` のような不正なリテラルを受理する。
- 同じ関数の長さ上限 39 は、IPv4-mapped 表記の最大 45 文字 (`0000:0000:0000:0000:0000:ffff:255.255.255.255`) を拒否する。RFC 4291 §2.5.5.2 の IPv4-mapped 表記は正当である。
- 同じ関数の `rsplit_once(':')` は、IPv4 末尾が `::` の直後に来る `[::1.2.3.4]` のような正当な形式を `ipv6_part = ":"` にしてしまい、セグメント数が 6 でないとして拒否する。RFC 3986 §3.2.2 の `IPv6address` は `"::" ls32` を許す。
- 呼び出しは `src/session/core.rs` の `Session::new_client` / `new_server` が呼ぶ `new` (`validate_setup_uri_format`) と `recv_control` 経由の `handle_peer_setup` (`validate_setup_uri_format`) の 2 経路である。
  `validate_setup_uri_format` は `SetupUriValidationError::Authority` を `SESSION_MALFORMED_AUTHORITY` に写す。
  `new` はセッションを作る前に `Err` を返し、`handle_peer_setup` は `fail` でセッションを閉じる。
- `tests/test_parameter.rs` の `uri_validation_success_cases` は `SetupOptions::decode` の往復テストであり、`decode` は URI 検証を行わない。`validate_uri_format` は `pub(crate)` で integration test から呼べないため、`tests/test_parameter.rs` では検証ロジックの受理・拒否を確認できない。
- 影響: 不正 AUTHORITY を受信しても閉じず、正当な AUTHORITY を受信すると誤って閉じる。相互接続障害になる。
- 既存の closed 0155 は example の `authority_parts` を対象にしており、本 issue の library SETUP 検証とは別経路である (重複ではない)。

## 設計方針

- `validate_ipv6addr` を `core::net::Ipv6Addr` の `FromStr` 実装 (`Ipv6Addr::from_str`) で置き換える。`core::net` は no_std で利用可能であり、本リポジトリの MSRV 1.93 で使える。`Ipv6Addr::from_str` は RFC 3986 §3.2.2 の `IPv6address` の文法 (IPv4 末尾を含む) を受理する。
- IPvFuture の既存検証 (`validate_ipv_future`) は維持する。`validate_ip_literal` の `v` / `V` の分岐は変更しない。
- 拒否時は従来どおり `SetupUriValidationError::Authority` を返し、session 層 (`validate_setup_uri_format`) が `SESSION_MALFORMED_AUTHORITY` に写す。
- `validate_ipv6addr` の受理・拒否は `src/parameter.rs` の `#[cfg(test)]` モジュールで直接テストする。`SetupUriValidationError` は `pub(crate)` であり、integration test からは呼べないためである。
- `Session` 経由の `SESSION_MALFORMED_AUTHORITY` は `tests/test_session/setup.rs` で `Session::new_client` (自側) と `recv_control` (peer) の両方をテストする。
- `tests/test_parameter.rs` の既存の codec 往復テスト (`authority_with_ipv6_port_roundtrip`) は維持する。`SetupOptions::decode` は URI 検証を行わないため、このファイルには受理・拒否テストを置かない。

## 完了条件

- `[1:::2]`、`[1:2:3:4:5:6:7:8:]`、`[:1:2:3:4:5:6:7:8]` を拒否し、`[::]`、`[::1]`、`[2001:db8::1]`、`[::1.2.3.4]`、`[::ffff:192.0.2.1]`、45 文字の IPv4-mapped 表記、IPvFuture の `[v1.fe80::]` を受理すること。
- `src/parameter.rs` の `#[cfg(test)]` モジュールに `validate_ipv6addr` (または `validate_uri_format`) の上記の受理・拒否テストがあること。
- `tests/test_session/setup.rs` に `Session::new_client` と `recv_control` 経由の `SESSION_MALFORMED_AUTHORITY` テストがあり、上記の拒否ケースと 45 文字の受理ケースを確認すること。
- `tests/test_parameter.rs` の既存の codec 往復テストが維持されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
