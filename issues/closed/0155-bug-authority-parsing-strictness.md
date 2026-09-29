# authority の解釈が RFC 3986 の host の規則より緩い

- Created: 2026-09-24
- Completed: 2026-09-30
- Branch: feature/fix-authority-parsing-strictness
- Polished: 2026-09-27

## 目的

URL の authority に含まれる不正な host と port を、名前解決や接続の前に拒否する。RFC 3986 §3.2.2 の `host = IP-literal / IPv4address / reg-name` を満たさない authority を受理すると、無関係な外部ホストへの接続や無意味なハンドシェイク試行になる。

## 現状

`examples/moqt-transport/src/lib.rs` の `authority_parts` は次の入力を受け付ける (実機で確認)。

- `moqt://[example.com]:4433/app`: `[` `]` の中が reg-name でも host `example.com` として受理する。RFC 3986 §3.2.2 の IP-literal は IPv6address または IPvFuture に限られる。`Resolved [example.com]:4433 to [2606:4700:10::ac42:93f3]:4433` のように外部ホストへ接続してしまう
- `moqt://127.0.0.1:0/app`: ポート 0 を受理し、`Resolved 127.0.0.1:0 to 127.0.0.1:0` の後にハンドシェイクがタイムアウトする
- `moqt://user@127.0.0.1:4433/app`: userinfo を分離せず host `user@127.0.0.1` として扱い、`failed to resolve 'user@127.0.0.1:4433'` という解決失敗になる。draft-ietf-moq-transport-21 §6.1 に userinfo の規定は無い
- `moqt://[fe80::1%25en0]:4433/app`: RFC 6874 の zone id 付き IPv6 リテラルは解決できない (macOS の getaddrinfo が受け付けない)

## 設計方針

- `[` `]` の中身を `Ipv6Addr` として解釈できない authority は拒否する。`[example.com]` のような reg-name や `[::1]x` のような余分な文字を弾く。IPvFuture (`[v1.fe80::]`) もこの判定で拒否される (RFC 3986 §3.2.2 は未知の version flag を持つ IP-literal を dereference するアプリケーションに 'address mechanism not supported' のエラーを返すことを推奨する)
- ポート 0 は拒否する。接続先として使えない (RFC 3986 §3.2.3 の `port = *DIGIT` は 0 も許すが、接続の観点では意味を成さない)
- userinfo は受理しない。`@` を含む authority は専用のメッセージで拒否し、host として名前解決に回さない。RFC 3986 §3.2 の authority 構文は userinfo を許すが、draft-ietf-moq-transport-21 §6.1 は userinfo に言及せず、RFC 3986 §3.2.1 は受け取った reference 中の
  userinfo を reject する選択を許す ("Applications may choose to ignore or reject such data when it is received as part of a reference")
- zone id 付き IPv6 リテラルは非対応とし、拒否する。`std::net::SocketAddr` / `Ipv6Addr` は zone id を解釈できず、接続層が `SocketAddr` 経由のため対応しない。RFC 3986 §3.2.2 も "This syntax does not support IPv6 scoped addressing zone identifiers." と明記する。非対応であることを `authority_parts` の doc に明記し、doc と実装を一致させる
- 拒否はすべて `TransportError::InvalidAuthority` とし、利用者向けの表示に `QUIC:` を付けない方針を維持する

## 完了条件

- `[example.com]` と `[::1]x` のような不正な IP-literal の authority が拒否されること
- ポート 0 の authority が拒否されること
- `user@host` の authority が名前解決に回らずに拒否されること
- zone id 付き IPv6 リテラルの authority が拒否され、非対応であることが `authority_parts` の doc に明記されていること
- 正当な authority (`127.0.0.1` / `127.0.0.1:4443` / `[::1]` / `relay.example.com` / `relay.example.com:443`) が従来どおり受理されること
- 追加した拒否ケースのテストが `examples/moqt-transport/src/lib.rs` にあること

## 解決方法

`examples/tokio-moq/src/lib.rs` の `authority_parts` で RFC 3986 §3.2.2 の host の規則と接続の観点から authority を検証するようにした。issue 本文の `examples/moqt-transport/` は現在の `examples/tokio-moq/` に当たる (ファイル構成の変更に追随させる)。

- `[` `]` の中身を `std::net::Ipv6Addr` として解釈できない authority を拒否する。`[example.com]` (reg-name)、`[v1.fe80::]` (IPvFuture)、`[fe80::1%25en0]` (zone id 付き IPv6 リテラル)、`[::1x]` が該当する。理由は `the host in '[' and ']' must be an IPv6 address`
- ポート 0 を拒否する。RFC 3986 §3.2.3 の `port = *DIGIT` は 0 も許すが接続先として使えない。理由は `port 0 cannot be used`
- `@` を含む authority を拒否する。判定は port の解釈より先に行い、`user:pass@host` を port のエラーとして報告しない。理由は `userinfo is not supported`
- `authority_parts` の doc に、受理する host (IPv6 リテラル / IPv4 リテラル / reg-name) と IPvFuture・zone id が非対応であること、userinfo を拒否する根拠 (draft-ietf-moq-transport-21 §6.1 が言及せず、RFC 3986 §3.2.1 が reject を許す) を明記した
- 拒否はすべて `TransportError::InvalidAuthority` のままで、利用者向けの表示に `QUIC:` を付けない方針を維持する

追加したテスト (`examples/tokio-moq/src/lib.rs` の `tests` モジュール):

- `authority_parts_rejects_non_ipv6_ip_literal`: `[example.com]` / `[example.com]:4443` / `[v1.fe80::]` / `[::1x]` を拒否し、理由がメッセージに含まれること
- `authority_parts_rejects_port_zero`: `127.0.0.1:0` / `[::1]:0` / `relay.example.com:0` を拒否すること
- `authority_parts_rejects_userinfo`: `user@127.0.0.1:4433` / `user:pass@relay.example.com` / `user@[::1]:4433` を拒否し、名前解決に回さないこと
- `authority_parts_rejects_ipv6_zone_id`: `[fe80::1%25en0]:4433` / `[fe80::1%en0]` を拒否すること
- `authority_parts_accepts_valid_authorities`: `127.0.0.1` / `127.0.0.1:4443` / `[::1]` / `[2001:db8::1]:4443` / `relay.example.com` / `relay.example.com:443` が従来どおり受理されること

`examples/README.md` の URL スキームの節に、拒否する authority の形式とエラーメッセージの方針を追記した。

実機確認 (`moq-pub` を実行し、拒否メッセージを確認した):

- `moqt://[example.com]:4433/app` → `Fatal: invalid server address '[example.com]:4433': the host in '[' and ']' must be an IPv6 address`
- `moqt://127.0.0.1:0/app` → `Fatal: invalid server address '127.0.0.1:0': port 0 cannot be used`
- `moqt://user@127.0.0.1:4433/app` → `Fatal: invalid server address 'user@127.0.0.1:4433': userinfo is not supported`
- `moqt://[fe80::1%25en0]:4433/app` → `Fatal: invalid server address '[fe80::1%25en0]:4433': the host in '[' and ']' must be an IPv6 address`
- `moqt://[::1]x:4433/app` → `Fatal: invalid server address '[::1]x:4433': unexpected text after ']'`
- 正当な authority (`127.0.0.1:4443` / `relay.example.com:443` / `[::1]:4443`) は名前解決の段階へ進むことを確認した

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。
