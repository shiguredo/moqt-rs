# URI fragment を :path / PATH に含めない

- Created: 2026-09-21
- Completed: 2026-09-24
- Branch: feature/fix-url-fragment-in-path
- Polished: 2026-09-22

## 目的

draft-ietf-moq-transport-21 §6.1.1 (Fragment Identifiers) は fragment をサーバーへ送信せず、クライアントがローカルで処理すると定める。

> Fragment identifiers MAY be used with moqt URIs.  The fragment is not transmitted to the server; it is processed locally by the client after establishing the MOQT session.

> `moqt://example.com/app#<type>:<value>`

RFC 9114 §4.3.1 も `:path` を path と query のみと規定する。

> ":path":  Contains the path and query parts of the target URI (the "path-absolute" production and optionally a ? character (ASCII 0x3f) followed by the "query" production; see Sections 3.3 and 3.4 of [URI].

現状は URL の `#` 以降が `:path` と PATH option に混入し、fragment の無い URL と同じ資源を指せない。
あわせて、`parse_url` の scheme 比較が RFC 3986 §3.1 の大文字小文字非区別に反する点も、同じ URL 解釈の修正として本 issue で扱う。

## 現状

- `examples/moqt-transport/src/lib.rs` の `split_authority_path` は authority を最初の `/` または `?` までとし、`#` を区切りに含めない。
  `moqt://example.com/app#type:value` は authority `example.com` / path `/app#type:value` になる。`moqt://example.com#type:value` では authority 自体が `example.com#type:value` になり、`host_from_authority` を通した SNI は `example.com#type` になる (`rfind(':')` で `:value` がポートとして落ちるため `#` 以降が全て入るわけではないが、fragment が混入する)。
- `examples/moqt-transport/src/webtransport.rs` の `WtClient::connect` は path を `ConnectRequest::new("https", &config.authority, path)` に渡すため、fragment が `:path` に入る。
- `examples/moqt-transport/src/moqt_client.rs` の `establish_quic` は `build_setup_options(Some(path), Some(authority), impl_name)` に path を渡すため、fragment が PATH option (0x01) に入る。draft-21 §9.1.2 (PATH) は PATH option の内容を次に限る。
  > When connecting to a server using a URI with the "moqt" scheme, the client MUST set the PATH option to the path-abempty portion of the URI; if query is present, the client MUST concatenate ?, followed by the query portion of the URI to the option.
- `examples/moqt-transport/src/lib.rs` の `parse_url` は `strip_prefix("moqt://")` / `strip_prefix("https://")` で scheme を比較するため、`MOQT://` のような大文字 scheme を拒否する。RFC 3986 §3.1 は scheme を大文字小文字非区別とする。
  > Although schemes are case-insensitive, the canonical form is lowercase and documents that specify schemes must do so with lowercase letters.
  > An implementation should accept uppercase letters as equivalent to lowercase in scheme names (e.g., allow "HTTP" as well as "http") for the sake of robustness but should only produce lowercase scheme names for consistency.

## 設計方針

- fragment を authority / path から分離して保持する。`ServerUrl` に `fragment: Option<MoqtFragment>` を追加し、
  draft-21 §6.1.1 の `<type>:<value>` を `type` と `value` に分けた型で表す。`#` を含む生文字列のまま保持しない。
- fragment は `#` の位置で切り出し、`#` 以降を最初の `:` で `type` と `value` に分ける。
  `:` が無い場合と `type` が空の場合と `type` が draft-21 §6.1.1 の文字種 (ASCII 小文字 / 数字 / ハイフン) に一致しない場合は
  `parse_url` をエラーにする。同節は次の 2 つの MUST を定める。
  "A moqt URI fragment MUST begin with a registered fragment type identifier, followed by a colon (:), followed by a type-specific value:" と
  "Fragment type identifiers MUST consist of ASCII lowercase letters, digits, and hyphens (a-z, 0-9, -)." である。
  同節に ABNF は無く本文の MUST で示される。
- fragment の中の `#` も拒否する。RFC 3986 §3.5 の `fragment = *( pchar / "/" / "?" )` は `#` を含まず `%23` が必要なため。
- `type` と `value` の規則は `moqt://` と `https://` のどちらにも適用する。根拠は次の 3 つ。
  draft-21 §6.2.1 が WebTransport の `https://` URI を
  "it constructs an https URI from the moqt URI by replacing the scheme with https." と moqt URI の scheme 置換として定める。
  §16.2 (Media Type Registration) が "Fragment identifiers for application/moqt follow the syntax defined in Section 6.1.1." と定める。
  RFC 3986 §3.5 が "Fragment identifier semantics are independent of the URI scheme" と定める。
  これにより WebTransport の `https://` でも同じ規則で検証し、`ServerUrl::fragment` に保持する。
- authority の有無の判定は fragment の解釈より先に行い、authority が空の URL (`moqt://` / `https:///app`) は fragment の形式エラーではなく authority のエラーとして報告する。
- `:path` と PATH option には fragment を含めない。example の接続経路では fragment の値を使わないため、値を使わないことをコメントで明示する。library の `msf::uri::parse_msf_uri` / `MsfFragment` (`src/msf/uri.rs`) は `msf:` fragment 専用で `msf:` 前置を必須とするため再利用せず、example 側の型として持つ。
- fragment を除いた path が空になる場合は `/` を補い、query のみのときに `/` を補う既存の扱いと揃える。
- scheme の比較を RFC 3986 §3.1 に合わせて大文字小文字非区別にする。正規化するのは scheme 部分だけで、ASCII 小文字化した scheme を比較と `Transport` の決定に使う。authority / path / query / fragment は入力の文字列のまま保持する (RFC 3986 §6.2.2.1 は scheme と host を大文字小文字非区別として小文字化を推奨するが、host を入力のまま使うのは DNS / SNI の非区別性に依存する設計判断とする)。
- `split_authority_path` の戻り値の形は変えず、fragment の分離は `parse_url` の中で行う。`split_authority_path` の既存テスト (query の扱い) は維持する。

## 完了条件

- fragment 付き URL で `:path` と PATH option に `#` 以降が含まれないことを固定するテストが追加されていること (`examples/moqt-transport/src/lib.rs` の `#[cfg(test)] mod tests`)
  - `moqt://example.com/app#type:value` の path が `/app` になる
  - `moqt://example.com/app?x=1#type:value` の path が `/app?x=1` になる
  - `https://example.com/app#type:value` の path が `/app` になる (WebTransport の `:path` に渡る https 分岐も対象)
  - `moqt://example.com#type:value` の authority が `example.com` になり、path が `/` になる
  - `parse_url` が返す path を `build_setup_options` に渡したとき、`SetupOptions::path()` の値に `#` 以降が入らないこと
- `type` と `value` が分離されるテストが追加されていること (`moqt://example.com/app#type:value` の fragment が `type` / `value` になる)
- `:` を含まない fragment (`moqt://example.com/app#typevalue`) と文字種に一致しない `type` (`moqt://example.com/app#Type:value` / `#:value` / `#ty_pe:value` / `#ty.pe:value`) と fragment 内の 2 個目の `#` (`#a:b#c:d`) が `parse_url` のエラーになるテストが追加されていること。エラーメッセージに URL が含まれること
- 数字とハイフンの `type` (`#a-b:value` / `#1:value` / `#a1-b2:value`) と空の `value` (`#type:`) が受理されるテストが追加されていること
- `https://` の fragment も `moqt://` と同じ規則で検証され、`https://example.com/app#type:value` の fragment が `type` / `value` に分離されるテストが追加されていること。`#section-2` / `#a:b#c` はエラーになること
- IPv6 リテラルの authority と大文字 scheme で fragment を分離でき、`host_from_authority` が fragment を含まない host を返すテストが追加されていること
- authority が空の URL (`moqt://` / `https:///app` / `moqt://?x=1#t:v`) のエラーメッセージに URL が含まれるテストが追加されていること
- 大文字 scheme (`MOQT://127.0.0.1:4443/path` / `HTTPS://127.0.0.1:4443/path`) が受理され、小文字 scheme と同じ `Transport` と authority / path に解決されること
- scheme 以外の成分が小文字化されないこと (`MOQT://127.0.0.1:4443/Path?X=Y` の path が `/Path?X=Y` のまま)
- fragment を持つ URL と持たない URL で、`parse_url` の authority と path が一致すること (fragment の有無が MOQT の経路に影響しないこと)

## 解決方法

`examples/moqt-transport/src/lib.rs` の `parse_url` で `#` 以降を fragment として分離し、`ServerUrl` に `fragment: Option<MoqtFragment>` を追加した。
fragment は `<type>:<value>` を `type` と `value` に分けた型で保持し、`#` を含む生文字列のまま保持しない。
分離後の path が SETUP の PATH option と WebTransport の `:path` に渡るため、fragment が接続経路に混入しなくなる。

fragment の扱いは次のとおり。

- `:` を含まない fragment と空の `type` は拒否する (draft-ietf-moq-transport-21 §6.1.1 の `:` の MUST)
- `type` が ASCII 小文字 / 数字 / ハイフン以外を含む場合は拒否する (同節の文字種の MUST)
- fragment 内の 2 個目の `#` は拒否する (RFC 3986 §3.5 の `fragment = *( pchar / "/" / "?" )` は `#` を含まず `%23` が必要)
- 数字とハイフンの `type` (`#a-b:value` / `#1:value`) と空の `value` (`#type:`) は受理する
- `https://` の fragment も同じ規則で検証する。§6.2.1 が WebTransport の https URI を moqt URI の scheme 置換と定め、
  §16.2 が application/moqt の fragment を §6.1.1 に従わせ、RFC 3986 §3.5 が fragment の意味は scheme に依存しないと定めるため

authority の有無は fragment の解釈より先に判定し、`moqt://` / `https:///app` / `moqt://?x=1#t:v` は authority のエラーとして報告する。
`parse_url` のエラーメッセージはすべて URL を含み、原因を特定できる。

scheme の比較は RFC 3986 §3.1 に従い大文字小文字非区別にした (`MOQT://` / `HTTPS://` を受理する)。
正規化するのは scheme だけで、authority / path / query / fragment は入力の文字列のまま保持する。

実機で確認した内容:

- ローカルの relay (sora-moq、4433) に対し `moqt://127.0.0.1:4433/app#type:value` の publisher と
  `MOQT://127.0.0.1:4433/app?x=1#type:value` の subscriber が接続し、SETUP と SUBSCRIBE_OK が成立して 200 フレームを描画した
- 不正な fragment は起動時に拒否される
  - `moqt://127.0.0.1:4433/app#a:b#c:d` は `invalid moqt URI fragment: ... ('#' inside a fragment must be percent-encoded)`
  - `moqt://127.0.0.1:4433/app#` は `invalid moqt URI fragment: ... (must be '<type>:<value>')`
  - `https://127.0.0.1:4433/app#section-2` は `invalid moqt URI fragment: ... (must be '<type>:<value>')`
- WebTransport の `:path` は実機で確認できなかった。ローカルの relay への WebTransport 接続が
  `webtransport setup error: reset_stream_at transport parameter not supported` で失敗するため
  (`issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md` で扱う)。https の分岐は `parse_url` の単体テストで担保する

`examples/README.md` の URL スキーム節、publisher / subscriber の `--url` ヘルプ、`CHANGES.md` を更新した。
library の `msf::uri::parse_msf_uri` は `msf:` fragment 専用で `msf:` 前置を必須とするため再利用していない。

追加したテスト (`moqt-example-transport` の lib ターゲットは 41 件から 64 件になった):

- `examples/moqt-transport/src/lib.rs` に 23 件
  - fragment の分離: path / query / authority / https / IPv6 リテラル / 大文字 scheme
  - `SetupOptions::path()` と SETUP のエンコード結果に `#` が入らないこと
  - `type` と `value` の分離、空の `value`、数字とハイフンの `type`
  - 拒否: `:` 無し、空の `type`、文字種違反 (`.` と `_` を含む)、fragment 内の 2 個目の `#`、空 authority
    (メッセージの完全一致で URL の含有も固定する)
