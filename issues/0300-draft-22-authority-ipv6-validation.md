# SETUP の AUTHORITY に含まれる IPv6 リテラルを RFC 3986 の IPv6address として検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-authority-ipv6-validation
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.1.1 (AUTHORITY) は AUTHORITY が RFC 3986 の URI formatting rules に従うことを要求し、"If an AUTHORITY option does not conform to these rules, the session MUST be closed with MALFORMED_AUTHORITY." と MUST を課す。現状の IPv6 リテラル検証は RFC 3986 §3.2.2 の `IPv6address`
より緩く、かつ IPv4-mapped 表記の正当な値を拒否するため、この MUST を満たしていない。

## 現状

- `src/parameter.rs` の `validate_ipv6addr` は `split(':')` の空セグメントを `filter` で捨て、`matches("::")` の回数を数えるだけで検証する。このため `[1:::2]`、`[1:2:3:4:5:6:7:8:]`、`[:1:2:3:4:5:6:7:8]` のような不正なリテラルを受理する。
- 同じ関数の長さ上限 39 は、IPv4-mapped 表記の最大 45 文字 (`0000:0000:0000:0000:0000:ffff:255.255.255.255`) を拒否する。RFC 4291 の IPv4-mapped 表記は正当である。
- 呼び出しは `src/session/core.rs` の `new` (`validate_setup_uri_format`) と `handle_peer_setup` (`validate_setup_uri_format`) の 2 経路で、失敗時は `SESSION_MALFORMED_AUTHORITY` でセッションを閉じる。
- 影響: 不正 AUTHORITY を受信しても閉じず、正当な AUTHORITY を受信すると誤って閉じる。相互接続障害になる。
- 既存の closed 0155 は example の `authority_parts` を対象にしており、本 issue の library SETUP 検証とは別経路である (重複ではない)。

## 設計方針

- `core::net::Ipv6Addr` 相当の厳密な検証に置き換える。no_std で使えない場合は、`::` の出現位置、空セグメント、セグメント数 (IPv4 末尾は 2 セグメント分)、IPv4 部分の妥当性、最大 45 文字を状態機械で検証する。
- IPvFuture の既存検証は維持する。
- 拒否時は従来どおり `SetupUriValidationError::Authority` を返し、session 層が `MALFORMED_AUTHORITY` に写す。

## 完了条件

- `[1:::2]`、`[1:2:3:4:5:6:7:8:]`、`[:1:2:3:4:5:6:7:8]` を拒否し、`[::]`、`[::1]`、`[2001:db8::1]`、45 文字の IPv4-mapped 表記を受理すること。
- `tests/test_parameter.rs` と `tests/test_session/setup.rs` に上記の受理・拒否テストがあること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
