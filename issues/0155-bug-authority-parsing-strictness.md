# authority の解釈が RFC 3986 の host の規則より緩い

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-authority-parsing-strictness
- Polished: {YYYY-MM-DD}

## 目的

URL の authority に含まれる不正な host と port を、名前解決や接続の前に拒否する。RFC 3986 §3.2.2 の `host = IP-literal / IPv4address / reg-name` を満たさない authority を受理すると、無関係な外部ホストへの接続や無意味なハンドシェイク試行になる。

## 現状

`examples/moqt-transport/src/lib.rs` の `authority_parts` は次の入力を受け付ける (実機で確認)。

- `moqt://[example.com]:4433/app`: `[` `]` の中が reg-name でも host `example.com` として受理する。RFC 3986 §3.2.2 の IP-literal は IPv6address または IPvFuture に限られる。`Resolved [example.com]:4433 to [2606:4700:10::ac42:93f3]:4433` のように外部ホストへ接続してしまう
- `moqt://127.0.0.1:0/app`: ポート 0 を受理し、`Resolved 127.0.0.1:0 to 127.0.0.1:0` の後にハンドシェイクがタイムアウトする
- `moqt://user@127.0.0.1:4433/app`: userinfo を分離せず host `user@127.0.0.1` として扱い、`failed to resolve 'user@127.0.0.1:4433'` という解決失敗になる。draft-ietf-moq-transport-21 §6.1 に userinfo の規定は無い
- `moqt://[fe80::1%25en0]:4433/app`: RFC 6874 の zone id 付き IPv6 リテラルは解決できない (macOS の getaddrinfo が受け付けない)

## 設計方針

- `[` `]` の中身を `Ipv6Addr` として解釈できない authority は拒否する。`[example.com]` のような reg-name と `[::1]x` のような余分な文字を弾く
- ポート 0 は拒否する。接続先として使えない
- userinfo は受理しない。`@` を含む authority は専用のメッセージで拒否し、host として名前解決に回さない
- zone id は `SocketAddr` として解釈できない。対応するか非対応を doc に明記するかを判断して実装し、`authority_parts` の doc と実装を一致させる
- 拒否はすべて `TransportError::InvalidAuthority` とし、利用者向けの表示に `QUIC:` を付けない方針を維持する

## 完了条件

- `[example.com]` と `[::1]x` のような不正な IP-literal の authority が拒否されること
- ポート 0 の authority が拒否されること
- `user@host` の authority が名前解決に回らずに拒否されること
- zone id 付き IPv6 リテラルの扱い (対応または非対応の明記) が決まり、実装と doc が一致していること
- 正当な authority (`127.0.0.1` / `127.0.0.1:4443` / `[::1]` / `relay.example.com` / `relay.example.com:443`) が従来どおり受理されること
- 追加した拒否ケースのテストが `examples/moqt-transport/src/lib.rs` にあること
