# tokio-moq

`moq-pub` / `moq-sub` が共有するトランスポート層のライブラリ。crates.io へは公開しない (`publish = false`)。

QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 / QMux の接続確立と、Sans I/O な `shiguredo_moqt::session::core::Session` を Tokio の I/O と結線する `MoqtClient` を提供する。`--url` と `--transport` の解釈もこのクレートが担う。`--transport auto` では、候補の経路を Happy Eyeballs v3 (draft-ietf-happy-happyeyeballs-v3-05) の考え方で競争させ、最初に確立した経路を使う。

ネイティブ QUIC と WebTransport over HTTP/3 は tobi-quic を使う。tobi-quic は Linux /
macOS / Windows に対応する。Multipath QUIC (RFC 10069) は Windows では使えない。

## 提供する範囲

- URL の解釈と接続パラメータの解決
  - `parse_url`：`moqt://` URI を `ServerUrl` にパースする。fragment は draft-ietf-moq-transport-23 §6.1.1 の構文で検証し、`msf` fragment は MSF fragment (`track-identifier [ "&" parameter-list ]`) として検証する
  - `resolve_transport`：`--transport` の指定と `msf` fragment の `connection` から `Transport` を解決する。`auto` は候補の展開を `connect` に委ねる
  - `resolve_namespace`：`--namespace` の指定と `msf` fragment の track-identifier から Track Namespace を解決する
  - `ServerUrl`：authority / path / fragment / MSF fragment の解釈結果 / C4M 認可トークン (`cat_tokens`) を保持する
- 接続確立：`quic` / `webtransport_h3` / `webtransport_h2` / `qmux` の各モジュールが TLS とハンドシェイクを行う。`happy-eyeballs` クレートの Sans I/O スケジューラが `--transport auto` の候補の並びと開始時刻を決め、`tokio-moq` のアダプタが接続 future とタイマーを駆動する
- `transport`：4 経路を統一的に扱う `SendStream` / `RecvStream` / `StreamHandle` / `StreamAcceptor` / `BidiStreamAcceptor`
- `happy_eyeballs::Config`：`--transport auto` と複数アドレスの競争に使う Connection Attempt Delay、最小値、最大値、倍率、Preferred Address Family Count を設定する
- `multipath`：Multipath QUIC (RFC 10069) のモード、インターフェースのフィルタ、パス数の上限、ネットワーク監視、keep-alive を設定する。tobi-quic の native QUIC 経路だけで使える (Windows では使えない)
- `moqt_client::MoqtClient`：control stream と bidi request stream を駆動する。`establish_quic` /
  `establish_wt` (wt-h3) / `establish_wt_h2` / `establish_qmux` (qmux) で確立し、`publish_track` /
  `subscribe_track` / `send_subscribe_ok` / `send_request_error` / `next_event` / `tick` などで
  MOQT メッセージをやり取りする (`establish_wt` は Windows ビルドでは提供しない)
- `moqt_client::DataPlaneHandle`：subgroup stream (`send_subgroup_header` / `send_subgroup_object`) と datagram (`send_object_datagram` / `recv_datagram`) を `Session` へ通知する
- `error::TransportError`：4 経路と MOQT メッセージのエラーを 1 つに統一する
- `metrics`：`tokio-metrics` のタスクメトリクスを一定間隔でログ出力するヘルパー

## セッション駆動

アプリケーションは `MoqtClient::next_event` を繰り返し呼び出す。`tokio::select!` で別の分岐が選ばれた場合も、クライアントは着手済みの制御処理を保持する。次に駆動したときに、同じ要求を再発行せず処理を続ける。定期的な `tick` または `tick_now` は、プロトコルの期限を進めるために必要である。

別タスクの `DataPlaneHandle` が制御メッセージの送信やセッションの終了を必要とするイベントを作ると、制御ストリームから何も届いていなくても、待機中の `next_event` が処理を再開する。

## URL スキーム

`--url` は `moqt://` の URI として解釈する。

- `moqt://host[:port]/path`：接続経路は `--transport` で選ぶ (`--transport` を省略した場合は `--url` の `msf` fragment の `connection` を使う)
- `moqt://host[:port]/path#msf:<track-identifier>`：`--namespace` を省略した場合の Track Namespace を track-identifier の namespace から取る

`port` を省略すると 443 を使う (draft-ietf-moq-transport-23 §6.1.2)。`host` は IP リテラル (例：`127.0.0.1` / `[2001:db8::1]`) と DNS 名の両方を受け付け、DNS 名は接続時に名前解決する。解決した接続先アドレスはログに出力される。

RFC 3986 §3.2.2 の host に一致しない authority は名前解決の前に拒否する。`[` `]` の中身は IPv6 アドレスに限るため `[example.com]` や IPvFuture (`[v1.fe80::]`)、zone id 付き IPv6 リテラル (`[fe80::1%25en0]`) はエラーになる。
ポート 0 と userinfo (`user@host`) もエラーになる。
userinfo を含まない authority の拒否メッセージには、拒否理由と入力した authority が含まれる。
userinfo を含む authority はパスワードなどの機密値を含み得るため、メッセージに authority を含めない。

`ServerUrl` の `Debug` も authority の userinfo (最後の `@` より前) を実値で表示しない。authority に `@` を含む場合 (空の userinfo を含む) は `authority` に `@` より後ろの host と port を表示し、`userinfo_bytes` に userinfo のバイト長を表示する。

URL と MSF fragment の構文検証エラーのメッセージには、入力 URL 全体とパラメータ値を含めない。query と fragment は認可トークンを含み得るため (draft-ietf-moq-msf-02 §11.5.2 / §11.1.1)、原因を特定できる非機密の成分 (scheme、authority / path などの成分名、track-identifier、パラメータ名、パラメータ位置) だけを示す。
`connection` や range の値変換エラーは機密値でないため、従来どおり値を示す。

DNS 名が複数のアドレスに解決される場合は、Happy Eyeballs v3 の考え方でアドレスを並べて接続を試す。IPv4 と IPv6 の両方に解決される `localhost` などでは、先頭の接続先が待ち受けていない場合にも次のアドレスへ進む。アドレスは先頭のファミリを優先しつつファミリを交互に並べ、同じファミリ内では解決器が返した順序を保つ。接続先のアドレスファミリに合わせてローカルソケットを選ぶため、IPv6 の接続先にも送信できる。
`--transport` が具体的な経路の場合は、その経路だけでアドレスを競争させる。`--transport auto` の場合は後述の候補を、経路とアドレスの組で並べて競争させる。

最初に確立した接続で MOQT の SETUP を交換し、残りの試行は採用時にキャンセルする。SETUP が失敗した場合は、次のアドレスや経路を試さずエラーを返す (SETUP の失敗を候補の失敗として扱わない)。WebTransport 経路では、Extended CONNECT と WebTransport のプロトコル交渉までがトランスポートの確立に含まれる。

query (`?key=value`) は SETUP の PATH option と WebTransport の `:path` にそのまま引き継がれる。

fragment (`#type:value`) は draft-ietf-moq-transport-23 §6.1.1 に従いサーバーへ送信せず、SETUP の PATH option と WebTransport の `:path` には含めない。§6.2.1 が WebTransport の https:// URI を moqt URI の scheme 置換と定め、§16.2 が application/moqt の fragment identifier を同節に従わせるため、WebTransport 経路でも同じ規則で扱う。
`type` は ASCII 小文字 / 数字 / ハイフンに限り、`:` を含まない fragment、空または規則に一致しない `type`、fragment 内の 2 個目の `#` (RFC 3986 §3.5 により `%23` が必要) はエラーになる。

`parse_url` は `type` が `msf` のときだけ値を MSF fragment (`track-identifier [ "&" parameter-list ]`) として検証し、`resolve_namespace` が track-identifier の示す namespace を `--namespace` 省略時の Track Namespace として使う。予約パラメータ `cat` の認可トークンは `ServerUrl::cat_tokens` に取り出す ([C4M 認可トークン](#c4m-認可トークン) を参照)。

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
(draft-ietf-moq-msf-02 §11.1.1 (Reserved fragment parameters))。`connection=wt` の
ときの既定は `wt-h3` (HTTP/2 にする場合は `--transport wt-h2` を明示する)。
`connection` と矛盾する `--transport`、`connection=q` と `connection=wt` の
併記、`connection=q` / `connection=wt` と `--transport qmux` の組み合わせはエラーになる。

`--transport auto` は候補の経路を Happy Eyeballs v3 で競争させ、最初に確立した経路を採用する。
`connection` が無ければ `quic` → `wt-h3` → `qmux` → `wt-h2` の順に候補を並べる。`connection=q` は `quic` だけ、`connection=wt`
は `wt-h3` → `wt-h2` を競争する。

| `--transport` | `Transport` | 接続経路 | 備考 |
| --- | --- | --- | --- |
| `quic` | `Transport::Quic` | QUIC 直接接続 | ALPN は `moqt-23` |
| `wt-h3` | `Transport::WtH3` | WebTransport over HTTP/3 | QUIC 上の ALPN は `h3` |
| `wt-h2` | `Transport::WtH2` | WebTransport over HTTP/2 | TCP+TLS 上の ALPN は `h2` |
| `qmux` | `Transport::Qmux` | QMux | TCP+TLS 上の ALPN は `moqt-23`。UDP が使えない環境向け。experimental |
| `auto` | `Transport::Auto` | Happy Eyeballs v3 による自動選択 | `connection` が無ければ `quic` → `wt-h3` → `qmux` → `wt-h2` を競争する。`connection=q` は `quic` だけ、`connection=wt` は `wt-h3` → `wt-h2` |

### Happy Eyeballs v3 による自動選択

`--transport auto` は draft-ietf-happy-happyeyeballs-v3-05 の Connection Attempts (§6) に従う。

- 候補は経路の優先順に並べ、各経路のアドレスはアドレスファミリを交互にした順序にする
  (§5.3)。先頭のファミリのアドレスを 1 個並べたあと、もう一方のファミリのアドレスを
  1 個挟む (Preferred Address Family Count は既定 1、設定可能)
- 最初の候補は即座に開始する。以降は Connection Attempt Delay (既定 250 ms、最小 100 ms、
  最大 2 s、倍率 1) ごとに次の候補を開始する (§6 / §10)
- 試行が失敗したときは、次の候補の開始を前倒しする。ただし直前の試行の開始から
  `minimum_connection_attempt_delay` (既定 100 ms) 未満では開始しない。仕様の MUST である
  10 ms も下限として守る
- いずれかの候補でトランスポートが確立した時点で、残りの試行をキャンセルする。成功判定は
  QUIC / TLS のハンドシェイクと、WebTransport の CONNECT およびプロトコル交渉までとする。
  MOQT の SETUP は勝者だけで交換する (§6.1 は成功判定をアプリケーションに委ねる)
- すべての候補が失敗した場合は、候補順で最も先頭の失敗を呼び出し元へ返す

候補の優先順は、QUIC を TCP より優先する §3 の前提に従う。同じ QUIC 系では層の少ない
native QUIC を WebTransport over HTTP/3 より先にし、TCP 系では QMux を WebTransport over
HTTP/2 より先にする。`connection=q` (draft-ietf-moq-msf-02 §11.1.1 が Native QUIC を MUST で
要求する) では QMux を候補に含めない。

Connection Attempt Delay などの値は `ConnectParams::happy_eyeballs`
(`happy_eyeballs::Config`) で変更できる。既定値は Connection Attempt Delay
250 ms、推奨最小値 100 ms、推奨最大値 2 s、倍率 1、Preferred Address Family Count 1
(いずれも設定可能) である。

本実装が対象とするのは接続試行の部分である。DNS の非同期解決 (§4) と SVCB/HTTPS レコードの
解釈 (§5.1 / §5.2 / §6.2 / §6.3) は `tokio::net::lookup_host` の解決結果を使うため対象外であり、
アドレスファミリの優先順は OS の解決器に委ねる。0-RTT とセッション再開は使わない。

一次資料は [`refs/happy/draft-ietf-happy-happyeyeballs-v3-05.txt`](../../refs/happy/draft-ietf-happy-happyeyeballs-v3-05.txt)
に置く。設計の詳細は [`docs/happy-eyeballs.md`](../../docs/happy-eyeballs.md) を参照する。

### QUIC 実装

ネイティブ QUIC (`--transport quic`) と WebTransport over HTTP/3 (`--transport wt-h3`) は
tobi-quic を使う。tobi-quic は Linux / macOS / Windows に対応する。

```bash
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example --transport wt-h3
```

### Multipath QUIC

Windows 以外のビルドでは、native QUIC 経路で Multipath QUIC (RFC 10069) を使える。既定は無効であり、`ConnectParams::multipath` (`tokio_moq::multipath::MultipathOptions`) の `mode` を `Off` 以外にしたときだけ `initial_max_path_id` を広告する。moq-pub / moq-sub では `--multipath` などで指定する。

- パスの使い方は `handover` / `aggregate` / `backup` から選ぶ
- `network_monitor_interval` を指定すると、インターフェースの増減に追従してパスを自動で開閉する
- `keep_alive_interval` を指定すると、待機パスを keep-alive で維持する。未指定の場合はアイドルタイムアウトから間隔を決める
- 接続確立後にパスの開設・検証・切り替えを観測し、ログに出す
- WebTransport、QMux、Windows では使えない。Multipath を有効にした接続は `--transport quic` を要求する

詳細は [docs/multipath.md](../../docs/multipath.md) を参照する。

WebTransport over HTTP/2 は draft-ietf-webtrans-http2-16 に従う。WebTransport over HTTP/3 は SETTINGS の codepoint と版選択、および `reset_stream_at` だけを draft-ietf-webtrans-http3-17 に合わせ、それ以外は draft-16 に従う。
どちらも Extended CONNECT でセッションを確立する。MOQT のプロトコル識別子 (`MOQT_PROTOCOL` = `moqt-23`) は `WT-Available-Protocols` で通知し、サーバーが `WT-Protocol` で選択しなければ接続を失敗させる (draft-ietf-moq-transport-23 §6.2)。

`wt-h2` は `WT_RESET_STREAM` capsule で reset するため、MoQ リレーの capsule 対応に依存する。

QMux 経路 (`qmux`) は experimental として扱う。QMux は順序保証された 1 本の双方向バイトストリーム (TLS over TCP) の上で QUIC version 1 のストリームと DATAGRAM を多重化する経路で (draft-ietf-quic-qmux-02)、UDP が使えない環境でも MOQT を試せるようにするためのものである。
QMux 自体は暗号化も ALPN のネゴシエーションも行わないため、ALPN は native QUIC と同じ `moqt-23` をこのクレートが張る (`wt-h2` の ALPN は `h2` のままである)。
DATAGRAM は経路での損失を下位の TCP が再送するため起きないが、TCP の再送で遅延が伸びる。また QMux は速やかに配送できない DATAGRAM の破棄を許すため (draft-ietf-quic-qmux-02 Section 6)、受信側のバッファが溢れると欠落しうる (破棄は `dropped_datagrams` として警告ログに出る)。QMux は `RESET_STREAM_AT` を実装しないため `reliable_size` は常に `None` になる。
送る DATAGRAM がピアの広告する `max_datagram_frame_size` か `max_record_size` (QMux はフレーム全体を両方と比較する) に収まらない場合、shiguredo_tokio_qmux の `send_datagram` はエラーを返す。音声は 20 ms ごとに送るためエラーログが大量にならないよう、接続確立時に 1 度だけ判定して警告ログ (英語) を出す (`--datagram-max-size` の既定 1160 の音声 Object は、QMux の DATAGRAM フレーム全体では 1163 バイトになる)。
`--cert` / `--insecure` の扱いは WebTransport over HTTP/2 経路と同じである (TCP + TLS の組み立てを共有し、ALPN だけが異なる)。省略時は OS のシステムルートストアで検証する。
この経路を選ぶと、接続の前に experimental である旨の警告ログ (英語) が出る。
実装は `qmux.rs` に閉じており、[qmux-rs](https://github.com/shiguredo/qmux-rs) の `shiguredo_qmux` と `shiguredo_tokio_qmux` を git の rev 固定で取り込む (タグが無く crates.io にも未公開のため)。

## C4M 認可トークン

MSF fragment (`#msf:<track-identifier>&cat=<token>`) の `cat` パラメータに C4M トークンを指定すると、SETUP の AUTHORIZATION_TOKEN (Token Type CAT) として送信する (draft-ietf-moq-msf-02 §11.1.1、draft-ietf-moq-c4m-02 §8.2.1)。
この経路はトークンのバイト列を運ぶだけで、トークンのデコード、署名 / MAC の検証、クレームや scope の認可、DPoP proof の検証は行わない。検証はトークンを受け取る側 (リレー) またはアプリケーションの責務である。
あわせて PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE の AUTHORIZATION_TOKEN としてもすべてのトークンを送信する (draft-ietf-moq-msf-02 §11.5.3 の MUST。SETUP に載せていても免除されない。`MoqtClient` が catalog を解析しないため track とトークンの対応付けは行わず、アクションごとの選別はしない)。

```bash
# C4M トークン付きで QUIC 接続する (シェルでは `&` を引用符で囲む)
cargo run -p moq-pub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video&cat=<base64 token>' --fake-capture-device
cargo run -p moq-sub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video&cat=<base64 token>'
```

`<base64 token>` は C4M トークンのバイト列 (CBOR エンコードされた CWT) を Base64 (RFC 4648 §4) または base64url (§5) で表した文字列で、パディングは省略できる。`%XX` の percent-encoding もデコードする。
トークンの検証 (署名 / クレーム / `moqt` クレームの認可判定) は MoQ リレーが行い、`parse_url` はデコードしたバイト列を `ServerUrl::cat_tokens` に保持し、送信時にそのまま載せる。DPoP バインディング (`cnf` / `catdpop`) を使うトークンに必要な DPoP proof の送信には対応しない。

`cat` は複数指定でき、すべてのトークンを SETUP と上記の制御メッセージの双方に載せる (同一トークンは 1 つに畳む)。MSF fragment が §11.1 の ABNF に一致しない場合と、`cat` が空または Base64 として不正な場合は `--url` の解釈でエラーになる。

## TLS 証明書

接続時の TLS は次の規則で検証する。

- `--cert` も `--insecure` も指定しない場合は、OS のシステムルートストアでサーバー証明書を検証する。rustls を使う経路 (native QUIC の tobi-quic、WebTransport over HTTP/3 / HTTP/2、QMux) は `rustls-native-certs` でシステムルートを読み込む
- `--cert` を指定した場合は、その CA 証明書だけをルートとして検証する
- `--insecure` を指定した場合だけ証明書検証をスキップする。接続時に警告ログが出る。`--cert` との同時指定はエラーになる
- システムルートを 1 つも読み込めない場合は接続をエラーにする。検証をスキップする経路へはフォールバックしない

自己署名証明書を使うローカル relay で試すときは、`--insecure` を明示する。

```bash
# 自己署名証明書を使うローカル relay へ接続する (開発用。証明書検証をスキップする)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --insecure --fake-capture-device
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example --insecure
```

本番環境では `--cert` で CA 証明書を指定するか、公開 CA の証明書を使ってシステムルートストアで検証すること。`--insecure` は使わないこと。

## テスト

`peer_harness` はテスト専用の in-process ハーネスである。`test-support` feature を有効にすると、他クレート (`moq-pub` / `moq-sub`) の統合テストから `tokio_moq::peer_harness` を参照できる。ハーネスは tobi-quic のサーバーを 127.0.0.1 に立て、in-process で MOQT peer を駆動する。モックやスタブは使わず、ライブラリの公開 API だけを使う。対象は QUIC 直接接続と QMux である。

QMux のハーネス (`QmuxTestPeerServer` / `TestPeer::wait_qmux_closed` など) はこの crate 自身の `#[cfg(test)]` テスト専用で、他クレートからは参照できない。`QmuxTestPeerServer` が TCP + TLS (ALPN `moqt-23`) を 127.0.0.1 に立て、`qmux` モジュールのテストが SETUP の交換、PUBLISH / SUBSCRIBE、subgroup stream と datagram の往復、ピアの STOP_SENDING の観測を実行で検証する。

Multipath QUIC のテストは tobi-quic ビルドで実行する。ループバックで実際に Multipath をネゴシエーションし、追加パスが `PathState::Active` になることと、その状態でストリームのデータが届くことを検証する。

```bash
cargo test -p tokio-moq multipath
```
