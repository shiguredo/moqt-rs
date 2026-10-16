# tokio-moq

`moq-pub` / `moq-sub` が共有するトランスポート層のライブラリ。crates.io へは公開しない (`publish = false`)。

QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 / QMux の接続確立と、Sans I/O な `shiguredo_moqt::session::core::Session` を Tokio の I/O と結線する `MoqtClient` を提供する。`--url` と `--transport` の解釈もこのクレートが担う。

## 提供する範囲

- URL の解釈と接続パラメータの解決
  - `parse_url`：`moqt://` URI を `ServerUrl` にパースする。fragment は draft-ietf-moq-transport-22 §6.1.1 の構文で検証し、`msf` fragment は MSF fragment (`track-identifier [ "&" parameter-list ]`) として検証する
  - `resolve_transport`：`--transport` の指定と `msf` fragment の `connection` から `Transport` を解決する
  - `resolve_namespace`：`--namespace` の指定と `msf` fragment の track-identifier から Track Namespace を解決する
  - `ServerUrl`：authority / path / fragment / MSF fragment の解釈結果 / C4M 認可トークン (`c4m_tokens`) を保持する
- 接続確立：`quic` / `webtransport_h3` / `webtransport_h2` / `qmux` の各モジュールが TLS とハンドシェイクを行う
- `transport`：4 経路を統一的に扱う `SendStream` / `RecvStream` / `StreamHandle` / `StreamAcceptor` / `BidiStreamAcceptor`
- `moqt_client::MoqtClient`：control stream と bidi request stream を駆動する。`establish_quic` / `establish_wt` (wt-h3) / `establish_wt_h2` / `establish_qmux` (qmux) で確立し、`publish_track` / `subscribe_track` / `send_subscribe_ok` / `send_request_error` / `next_event` / `tick` などで MOQT メッセージをやり取りする
- `moqt_client::DataPlaneHandle`：subgroup stream (`send_subgroup_header` / `send_subgroup_object`) と datagram (`send_object_datagram` / `recv_datagram`) を `Session` へ通知する
- `error::TransportError`：4 経路と MOQT メッセージのエラーを 1 つに統一する
- `metrics`：`tokio-metrics` のタスクメトリクスを一定間隔でログ出力するヘルパー

## URL スキーム

`--url` は `moqt://` の URI として解釈する。

- `moqt://host[:port]/path`：接続経路は `--transport` で選ぶ (`--transport` を省略した場合は `--url` の `msf` fragment の `connection` を使う)
- `moqt://host[:port]/path#msf:<track-identifier>`：`--namespace` を省略した場合の Track Namespace を track-identifier の namespace から取る

`port` を省略すると 443 を使う (draft-ietf-moq-transport-22 §6.1.2)。`host` は IP リテラル (例：`127.0.0.1` / `[2001:db8::1]`) と DNS 名の両方を受け付け、DNS 名は接続時に名前解決する。解決した接続先アドレスはログに出力される。

RFC 3986 §3.2.2 の host に一致しない authority は名前解決の前に拒否する。`[` `]` の中身は IPv6 アドレスに限るため `[example.com]` や IPvFuture (`[v1.fe80::]`)、zone id 付き IPv6 リテラル (`[fe80::1%25en0]`) はエラーになる。ポート 0 と userinfo (`user@host`) もエラーになる。エラーメッセージには拒否理由と入力した authority が含まれる。

DNS 名が複数のアドレスに解決される場合は、解決順に接続を試し、確立できたアドレスを採用する。IPv4 と IPv6 の両方に解決される `localhost` などで、先頭の接続先が待ち受けていない場合にも次のアドレスへ進む。接続の試行は `--transport` で選んだ経路の確立までを単位とし、確立後の SETUP の失敗では次のアドレスを試さない。接続先のアドレスファミリに合わせてローカルソケットを選ぶため、IPv6 の接続先にも送信できる。

query (`?key=value`) は SETUP の PATH option と WebTransport の `:path` にそのまま引き継がれる。

fragment (`#type:value`) は draft-ietf-moq-transport-22 §6.1.1 に従いサーバーへ送信せず、SETUP の PATH option と WebTransport の `:path` には含めない。§6.2.1 が WebTransport の https:// URI を moqt URI の scheme 置換と定め、§16.2 が application/moqt の fragment identifier を同節に従わせるため、WebTransport 経路でも同じ規則で扱う。
`type` は ASCII 小文字 / 数字 / ハイフンに限り、`:` を含まない fragment、空または規則に一致しない `type`、fragment 内の 2 個目の `#` (RFC 3986 §3.5 により `%23` が必要) はエラーになる。

`parse_url` は `type` が `msf` のときだけ値を MSF fragment (`track-identifier [ "&" parameter-list ]`) として検証し、`resolve_namespace` が track-identifier の示す namespace を `--namespace` 省略時の Track Namespace として使う。予約パラメータ `c4m` の認可トークンは `ServerUrl::c4m_tokens` に取り出す ([C4M 認可トークン](#c4m-認可トークン) を参照)。

予約パラメータ `connection` は `--transport` 省略時の接続種別として使い、`--transport` と矛盾する場合はエラーになる ([トランスポート](#トランスポート) を参照)。`msf` 以外の `type` の値は構文 (§6.1.1) の検証だけを行い、解釈しない。

scheme は RFC 3986 §3.1 に従い大文字小文字を区別しない (`MOQT://` も受理する)。`moqt://` 以外の scheme はエラーになる。authority / path / query / fragment は入力の大文字小文字をそのまま使う。

`--namespace` を省略した場合の Track Namespace の決定は次のとおりである。

- `--namespace` を指定した場合はそちらを使う
- `--namespace` を省略した場合は `msf` fragment の track-identifier (§11.1.2 の namespace-name 表現。`-` が namespace フィールドの区切り、`--` が namespace と track name の境界、`%XX` はデータバイト) が示す namespace を使う
- `--namespace` を省略したときに `--url` に `msf` fragment が無い場合、`msf` 以外の fragment の場合、`msf` fragment の namespace が 0 フィールドの場合はエラーになる

`msf` fragment の track name は使わず、配信する Track Name は moq-pub の `--track-name` (既定 `video`) で指定する。

## トランスポート

接続経路は `--transport` で選ぶ。`--transport` を省略した場合は `--url` の `msf` fragment の
`connection` (`q`: native QUIC / `wt`: WebTransport) を使い、それも無ければ `quic` になる
(draft-ietf-moq-msf-01 §11.1.1 (Reserved fragment parameters))。WebTransport 経路と QMux 経路は
experimental のため、`--transport` も `connection` も無いときの既定にはしない。`connection=wt` の
ときの既定は `wt-h3` (HTTP/2 にする場合は `--transport wt-h2` を明示する)。`connection` と矛盾する
`--transport`、`connection=q` と `connection=wt` の併記、`connection=q` / `connection=wt` と
`--transport qmux` の組み合わせはエラーになる。

| `--transport` | `Transport` | 接続経路 | 備考 |
| --- | --- | --- | --- |
| `quic` | `Transport::Quic` | QUIC 直接接続 | ALPN は `moqt-22` |
| `wt-h3` | `Transport::WtH3` | WebTransport over HTTP/3 | QUIC 上の ALPN は `h3`。experimental |
| `wt-h2` | `Transport::WtH2` | WebTransport over HTTP/2 | TCP+TLS 上の ALPN は `h2`。experimental |
| `qmux` | `Transport::Qmux` | QMux | TCP+TLS 上の ALPN は `moqt-22`。UDP が使えない環境向け。experimental |

WebTransport 経路では draft-ietf-webtrans-http3-16 / draft-ietf-webtrans-http2-15 に従い、Extended CONNECT でセッションを確立する。MOQT のプロトコル識別子 (`MOQT_PROTOCOL` = `moqt-22`) は `WT-Available-Protocols` で通知し、サーバーが `WT-Protocol` で選択しなければ接続を失敗させる (draft-ietf-moq-transport-22 §6.2)。

WebTransport 経路 (`wt-h3` / `wt-h2`) は experimental として扱う。draft-ietf-webtrans-http3-16 §4.4 は WebTransport のデータストリームを reset するとき、WebTransport ヘッダ以上の Reliable Size を持つ `RESET_STREAM_AT` を使うことを MUST とする。
ただし example が使う s2n-quic は `RESET_STREAM_AT` を送出できず、`reset_stream_at` transport parameter も広告しない。
このため `wt-h3` の reset は `RESET_STREAM` へのフォールバックになり、WebTransport ヘッダが確実に届く保証が無い。`wt-h2` は `WT_RESET_STREAM` capsule で reset するため `RESET_STREAM_AT` は不要だが、MoQ リレーの capsule 対応に依存する。
reset を伴う経路 (request stream の cancel や、送信中の subgroup / data stream の中断) は MoQ リレーによっては成立しない。
この経路を選ぶと、接続の前に experimental である旨の警告ログ (英語) が出る。

QMux 経路 (`qmux`) は experimental として扱う。QMux は順序保証された 1 本の双方向バイトストリーム (TLS over TCP) の上で QUIC version 1 のストリームと DATAGRAM を多重化する経路で (draft-ietf-quic-qmux-02)、UDP が使えない環境でも MOQT を試せるようにするためのものである。
QMux 自体は暗号化も ALPN のネゴシエーションも行わないため、ALPN は native QUIC と同じ `moqt-22` をこのクレートが張る (`wt-h2` の ALPN は `h2` のままである)。
DATAGRAM は経路での損失を下位の TCP が再送するため起きないが、TCP の再送で遅延が伸びる。また QMux は速やかに配送できない DATAGRAM の破棄を許すため (draft-ietf-quic-qmux-02 Section 6)、受信側のバッファが溢れると欠落しうる (破棄は `dropped_datagrams` として警告ログに出る)。QMux は `RESET_STREAM_AT` を実装しないため `reliable_size` は常に `None` になる。
送る DATAGRAM がピアの広告する `max_datagram_frame_size` に収まらない場合、QMux は送信の失敗を呼び出し側へ返さず音声が無言で落ちる。このため接続確立時に警告ログ (英語) を出す (`--datagram-max-size` の既定 1160 の音声 Object は、QMux の DATAGRAM フレーム全体では 1163 バイトになる)。
`--cert` の扱いは WebTransport over HTTP/2 経路と同じで、省略すると証明書検証をスキップする (TCP + TLS の組み立てを共有し、ALPN だけが異なる)。
この経路を選ぶと、接続の前に experimental である旨の警告ログ (英語) が出る。
実装は `qmux.rs` に閉じており、[qmux-rs](https://github.com/shiguredo/qmux-rs) の `shiguredo_qmux` と `tokio-qmux` を git の rev 固定で取り込む (タグが無く crates.io にも未公開のため)。

## C4M 認可トークン

MSF fragment (`#msf:<track-identifier>&c4m=<token>`) の `c4m` パラメータに C4M トークンを指定すると、SETUP の AUTHORIZATION_TOKEN (Token Type CAT) として送信する (draft-ietf-moq-msf-01 §11.1.1、draft-ietf-moq-c4m-01 §7.1.1)。
あわせて PUBLISH / SUBSCRIBE / REQUEST_UPDATE の AUTHORIZATION_TOKEN としてもすべてのトークンを送信する (draft-ietf-moq-msf-01 §11.4.3 の MUST。SETUP に載せていても免除されない。`MoqtClient` が catalog を解析しないため track とトークンの対応付けは行わず、アクションごとの選別はしない)。

```bash
# C4M トークン付きで QUIC 接続する (シェルでは `&` を引用符で囲む)
cargo run -p moq-pub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video&c4m=<base64 token>' --fake-capture-device
cargo run -p moq-sub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video&c4m=<base64 token>'
```

`<base64 token>` は C4M トークンのバイト列 (CBOR エンコードされた CWT) を Base64 (RFC 4648 §4) または base64url (§5) で表した文字列で、パディングは省略できる。`%XX` の percent-encoding もデコードする。
トークンの検証 (署名 / クレーム / `moqt` クレームの認可判定) は MoQ リレーが行い、`parse_url` はデコードしたバイト列を `ServerUrl::c4m_tokens` に保持し、送信時にそのまま載せる。DPoP バインディング (`cnf` / `catdpop`) を使うトークンに必要な DPoP proof の送信には対応しない。

`c4m` は複数指定でき、すべてのトークンを SETUP と上記の制御メッセージの双方に載せる (同一トークンは 1 つに畳む)。MSF fragment が §11.1 の ABNF に一致しない場合と、`c4m` が空または Base64 として不正な場合は `--url` の解釈でエラーになる。

## TLS 証明書

接続時の TLS は `--cert` の CA 証明書で検証する。`--cert` を省略すると証明書検証をスキップする。

本番環境では `--cert` で CA 証明書を指定して証明書検証を有効にすること。

## テスト

`peer_harness` (テスト専用、`#[cfg(test)]`) が s2n-quic のサーバーを 127.0.0.1 に立て、in-process で MOQT peer を駆動する。モックやスタブは使わず、ライブラリと example の公開 API だけを使う。対象は QUIC 直接接続と QMux である。

QMux は `QmuxTestPeerServer` が TCP + TLS (ALPN `moqt-22`) を 127.0.0.1 に立て、`qmux` モジュールのテストが SETUP の交換、PUBLISH / SUBSCRIBE、subgroup stream と datagram の往復、ピアの STOP_SENDING の観測を実行で検証する。
