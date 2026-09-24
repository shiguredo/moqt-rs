# ポート省略 URL で既定ポート 443 を使う

- Created: 2026-09-21
- Completed: 2026-09-24
- Branch: feature/fix-url-default-port
- Polished: 2026-09-22

## 目的

draft-ietf-moq-transport-21 §6.1.2 (Dereferencing a MOQT URI) は、URI でポートが省略された場合に既定ポート 443 を使うと規定する。

> If the port is omitted in the URI, a default port of 443 is used.

現状はポートを省略した URL を解釈できず、接続先を決める段階で必ず失敗する (`parse_url` 自体は成功する)。`moqt://relay.example.com/app` のような URL で publisher / subscriber が接続できるようにする。

## 現状

- `examples/moqt-transport/src/lib.rs` の `parse_url` は scheme を取り除いた残りを `split_authority_path` に渡すだけで、authority にポートがあるかを見ない。`ServerUrl::authority` には URL の authority がそのまま入る。
- `examples/moqt-transport/src/quic.rs` の `connect` は `authority.parse::<std::net::SocketAddr>()` で接続先を作る。ポートが無い authority はパースに失敗し、`TransportError::Quic` の "invalid server address" になる。
- `examples/moqt-publisher/src/pipeline.rs` の `run` (`Transport::WebTransport` 分岐) と `examples/moqt-subscriber/src/pipeline.rs` の `run` (`Transport::WebTransport` 分岐) も同じく `config.url.authority.parse::<std::net::SocketAddr>()` を使い、"invalid server address" で失敗する。
- ポートを補うだけではホスト名の authority は解決できない。`std::net::SocketAddr` は IP アドレスしか表せないため `relay.example.com:443` もパースに失敗する。example には DNS 解決の経路が無く、`examples/README.md` の例は `moqt://127.0.0.1:4443` のように IP リテラルとポートを明示する形になっている。

## 設計方針

- `examples/moqt-transport/src/lib.rs` に authority から (host, port) を取り出す純関数を追加する。`host_from_authority` と同じ規則で host を取り出し (IPv6 リテラルは `[` から `]` までを host とする)、port は `:` の後ろの数字、無ければ 443 を既定値にする。
- 接続先 `SocketAddr` の解決関数を追加し、決定を 1 箇所に集約する。host が IP リテラルなら `SocketAddr` に直接パースし、ホスト名なら `tokio::net::lookup_host` で解決して最初の結果を使う。
  host に `:` を含む (IPv6 リテラル) 場合は `SocketAddr` へパースする前に `[` `]` で囲む (`host_from_authority` はブラケットを外して返すため、そのまま連結すると `2001:db8::1:443` になり失敗する)。
- 純関数と解決関数を分ける。純関数は単体テストで 443 の補完と不正な authority の拒否を固定する。解決関数の IP リテラル経路は単体テストで固定し、ホスト名経路は `localhost` の解決テストと実機確認で確認する (モックやスタブは使わない)。
- 次の authority は拒否する。host が空 (`:4443`)、ブラケット無しの IPv6 リテラル (`::1` は最後の `:` で分けると host `:` / port `1` になる)、IPv6 リテラルの閉じ括弧欠如 (`[::1`)、ポートの区切りがあるのに数字が続かない場合 (`127.0.0.1:` / `127.0.0.1:abc`)。draft-ietf-moq-transport-21 §6.1 (MOQT URI Scheme) は authority の host 部分が空であることを MUST NOT と定める。
- 接続先のアドレスファミリに合わせてローカルソケットを選ぶ。`0.0.0.0:0` 固定では IPv4 ソケットから IPv6 宛に送信できず、IPv6 に解決された接続先へパケットを送れない。
- authority の解釈失敗と名前解決失敗はトランスポート種別に依存しないため、QUIC 由来のエラーとは別のエラーとして返し、利用者向けの表示に `QUIC:` を付けない。
- 解決したアドレスは接続前に `tracing::info!` でログに出す (実機確認で接続先を確認するため)。
- `quic::connect` と publisher / subscriber の pipeline はこの結果を使い、`authority.parse::<SocketAddr>()` の重複を残さない。`quic::connect` は authority ではなく解決済みの `SocketAddr` を受け取る形にする。
- `ServerUrl::authority` は URL の authority をそのまま保持する。SETUP の AUTHORITY option (`build_setup_options`) と WebTransport CONNECT の `:authority` (`ClientConfig::authority`) には元の URL の authority を渡し続け、省略されたポートを勝手に補わない。
- ポートを明示した URL の既存挙動は変えない。
- `examples/README.md` と publisher / subscriber の `--url` ヘルプは URL スキームの記述 (`moqt://host:port/path` / `https://host:port/path`) と「DNS 名は名前解決を行わないため接続できない」という記述が実装と矛盾するため、ポート省略時は 443 になり、ホスト名も解決するようになる実態に合わせて更新する。

## 完了条件

- authority から (host, port) を取り出す純関数の単体テストが追加され、次が固定されていること
  - `moqt://127.0.0.1/app` の authority から host `127.0.0.1` / port 443 が得られる
  - `https://[2001:db8::1]/app` の authority から host `2001:db8::1` / port 443 が得られ、接続先文字列が `[2001:db8::1]:443` になる
  - ポート付きの authority (`127.0.0.1:4443`) は host `127.0.0.1` / port 4443 が得られる
  - ホスト名の authority (`relay.example.com`) は host `relay.example.com` / port 443 が得られる
- 解決関数の単体テストが追加され、次が固定されていること (モックやスタブは使わない)
  - IP リテラル (`127.0.0.1` / `127.0.0.1:4443` / `[::1]`) が、既定ポートの補完または明示ポートの維持をした `SocketAddr` に解決される
  - `localhost` が名前解決されてポート 443 になり、ループバックに解決される
  - 不正な authority (`::1` / `127.0.0.1:abc`) は解決せずにエラーになる
- 接続先のアドレスファミリに合わせてローカルソケットを選ぶ関数の単体テストが追加されていること
- 不正な authority (`""` / `:4443` / `[::1` / `[::1]x` / `2001:db8::1` / `127.0.0.1:` / `127.0.0.1:abc` / `127.0.0.1:65536`) が拒否されるテストが追加されていること
- authority の解釈失敗が `TransportError::InvalidAuthority` として返り、トランスポート層と publisher / subscriber の表示に `QUIC:` が付かないテストが追加されていること (名前解決失敗も同じ扱いにする)
- `examples/README.md` の URL スキーム節と実行例、publisher / subscriber の `--url` ヘルプの文字列 (`cli.rs` の `.doc()` 定義) が、ポート省略時は 443 になりホスト名も解決する実態に一致していること
  - 両バイナリの `--help` が `missing '--url' option` で終了する既存の不具合 (現状では `--help` からはヘルプを表示できない) は本 issue の対象外とし、`issues/0151-bug-moqt-example-help-flag.md` で扱う
- publisher / subscriber の両経路で、ポート省略 URL からの接続が成立すること。確認手順を次に示す。relay は本リポジトリに含まれないため別途用意する。
  1. relay を 443 で待ち受ける。443 は特権ポートのため、待ち受けられない環境では 443 から relay の待ち受けポートへの転送を用意する (URL にポートを明示する代替では既定ポート 443 の確認にならない)
  2. `moqt-publisher --url moqt://127.0.0.1/app` を起動し、ログの接続先が `127.0.0.1:443` になることと SETUP が成立することを確認する。
     解決後のアドレスを含むログが接続前に出ること (変更前の `Connected to {authority}` は authority 文字列のみで、解決後のアドレスを含まないため不十分)
  3. `moqt-subscriber --url moqt://127.0.0.1/app` でも同じ確認を行う
  4. ホスト名経路は `moqt://localhost/app` を指定し、ログの解決後アドレスが `127.0.0.1:443` または `[::1]:443` になることを確認する (接続の成否は relay の待ち受けアドレスに依存する)
- `Transport::WebTransport` 分岐も同じアドレス決定関数を通ること。WebTransport 経路の実接続確認は [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) (RESET_STREAM_AT 非対応) の解消後に relay の応答次第で行う。

## 解決方法

`examples/moqt-transport/src/lib.rs` に authority から host と port を取り出す純関数 (`authority_parts`) と、接続先を解決する関数 (`resolve_socket_addr`) を追加した。
ポートが省略された authority は draft-ietf-moq-transport-21 §6.1.2 の既定ポート 443 を使う。
ホスト名は `tokio::net::lookup_host` で解決して最初の結果を使い、解決したアドレスを接続前に `Resolved {authority} to {addr}` としてログに出す。

`quic::connect` は authority ではなく解決済みの `SocketAddr` を受け取るようにし、publisher / subscriber の pipeline は transport 分岐の前に 1 回だけ解決する。
QUIC と WebTransport の両方が同じ `SocketAddr` を使う。
接続先のアドレスファミリに合わせてローカルソケットを選ぶようにし (`local_bind_addr`)、IPv6 に解決された接続先へも送信できるようにした。
SETUP の AUTHORITY option と WebTransport の `:authority` には URL の authority をそのまま渡す。

次の authority は `invalid server address` として拒否する。

- host が空 (`""` / `:4443`)
- IPv6 リテラルの閉じ括弧が無い (`[::1`)、または `]` の直後がポート区切りでない (`[::1]x`)
- ブラケット無しの IPv6 リテラル (`::1` / `2001:db8::1`)
- ポートの区切りがあるのに数字が続かない (`127.0.0.1:` / `127.0.0.1:abc` / `127.0.0.1:65536`)

authority の解釈失敗と名前解決失敗は `TransportError::InvalidAuthority` / `TransportError::ResolutionFailed` として返し、利用者向けの表示に `QUIC:` を付けない。

実機で確認した内容:

- `moqt://sora-moq.shiguredo.co.jp/app` (ポート省略) で publisher / subscriber の両方が既定ポート 443 に接続し、SETUP が成立した。
  名前解決は `[2600:3c18::2000:8cff:fed9:2055]:443` になり、subscriber は SUBSCRIBE_OK の後に 700 フレームを描画した。
  IPv6 に解決された接続先へ送信できたのは `local_bind_addr` の効果である
- 443 を待ち受ける relay をこの環境 (非 root) では用意できないため、ローカルの relay (sora-moq) を 4433 で待ち受けて
  `moqt://127.0.0.1:4433/app` の SETUP 成立も確認した
- `moqt://[::1]:5556/app` の QUIC Initial が IPv6 の listener に到達することを確認した (修正前は 1 パケットも届かなかった)
- 不正な authority は `Fatal: invalid server address '::1': IPv6 literal must be enclosed in '[' and ']'` のように表示される

`examples/README.md` の URL スキーム節と実行例、publisher / subscriber の `--url` ヘルプの文字列、`CHANGES.md` を実態に合わせて更新した。両バイナリの `--help` がヘルプを表示しない既存の不具合は `issues/0151-bug-moqt-example-help-flag.md` で扱う。

追加したテスト (lib ターゲットは 41 件が成功):

- `examples/moqt-transport/src/lib.rs` に 21 件
  - 純関数: 443 の補完 (IPv4 / IPv6 / ホスト名)、明示ポートの維持、`parse_url` との連結、拒否条件 6 件
  - 解決関数: IPv4 / IPv6 リテラルと `localhost` の解決、不正 authority の拒否
  - `local_bind_addr` のファミリ選択、`TransportError` の表示に `QUIC:` を付けないこと
- `examples/moqt-publisher/src/error.rs` と `examples/moqt-subscriber/src/error.rs` に `From<TransportError>` の写像テスト (各 2 件)
