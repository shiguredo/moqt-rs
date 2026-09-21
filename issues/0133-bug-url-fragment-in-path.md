# URI fragment を :path / PATH に含めない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-url-fragment-in-path
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.1.1 (Fragment Identifiers) は fragment をサーバーへ送信せず、クライアントがローカルで処理すると定める。

> Fragment identifiers MAY be used with moqt URIs.  The fragment is not transmitted to the server; it is processed locally by the client after establishing the MOQT session.

> moqt://example.com/app#&lt;type&gt;:&lt;value&gt;

RFC 9114 §4.3.1 も `:path` を path と query のみと規定する。

> ":path":  Contains the path and query parts of the target URI (the "path-absolute" production and optionally a ? character (ASCII 0x3f) followed by the "query" production; see Sections 3.3 and 3.4 of [URI].

現状は URL の `#` 以降が `:path` と PATH option に混入し、fragment の無い URL と同じ資源を指せない。

## 現状

- `examples/moqt-transport/src/lib.rs` の `split_authority_path` は authority を最初の `/` または `?` までとし、`#` を区切りに含めない。
  `moqt://example.com/app#type:value` は authority `example.com` / path `/app#type:value` になる。`moqt://example.com#type:value` では authority 自体が `example.com#type:value` になり、`host_from_authority` を通した SNI にも `#` 以降が入る。
- `examples/moqt-transport/src/webtransport.rs` の `WtClient::connect` は path を `ConnectRequest::new("https", &config.authority, path)` に渡すため、fragment が `:path` に入る。
- `examples/moqt-transport/src/moqt_client.rs` の `establish_quic` は `build_setup_options(Some(path), Some(authority), impl_name)` に path を渡すため、fragment が PATH option (0x01) に入る。draft-21 §9.1.2 (PATH) は PATH option の内容を次に限る。
  > When connecting to a server using a URI with the "moqt" scheme, the client MUST set the PATH option to the path-abempty portion of the URI; if query is present, the client MUST concatenate ?, followed by the query portion of the URI to the option.
- `examples/moqt-transport/src/lib.rs` の `parse_url` は `strip_prefix("moqt://")` / `strip_prefix("https://")` で scheme を比較するため、`MOQT://` のような大文字 scheme を拒否する。RFC 3986 §3.1 は scheme を大文字小文字非区別とする。
  > Although schemes are case-insensitive, the canonical form is lowercase and documents that specify schemes must do so with lowercase letters.
  > An implementation should accept uppercase letters as equivalent to lowercase in scheme names (e.g., allow "HTTP" as well as "http") for the sake of robustness but should only produce lowercase scheme names for consistency.

## 設計方針

- fragment を authority / path から分離して保持する。`ServerUrl` に fragment を持たせ、draft-21 §6.1.1 の `<type>:<value>` を型で表す。`#` を含む生文字列のまま保持しない。
- `:path` と PATH option には fragment を含めない。fragment を使う経路は現状無いため、値を使わないことをコメントで明示し、将来 MSF URI などで使うときの入口を型として残す。
- fragment を除いた path が空になる場合は `/` を補い、query のみのときに `/` を補う既存の扱いと揃える。
- scheme の比較を RFC 3986 §3.1 に合わせて大文字小文字非区別にし、内部では小文字に正規化する。`Transport` の決定は正規化後の scheme で行う。
- `split_authority_path` の戻り値の形は変えず、fragment の分離は `parse_url` の中で行う。`split_authority_path` の既存テスト (query の扱い) は維持する。

## 完了条件

- fragment 付き URL で `:path` と PATH option に `#` 以降が含まれないことを固定するテストが追加されていること
  - `moqt://example.com/app#type:value` の path が `/app` になる
  - `moqt://example.com/app?x=1#type:value` の path が `/app?x=1` になる
  - `moqt://example.com#type:value` の authority が `example.com` になる
  - `moqt://example.com/app#type:value` の path を `build_setup_options` に渡したとき、PATH option の値に `#` 以降が入らないこと
- 大文字 scheme (`MOQT://127.0.0.1:4443/path` / `HTTPS://127.0.0.1:4443/path`) が受理され、小文字 scheme と同じ `Transport` と authority / path に解決されること
- fragment を持つ URL と持たない URL で、`parse_url` の authority と path が一致すること (fragment の有無が MOQT の経路に影響しないこと)
