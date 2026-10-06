# MoQ サンプル

Media over QUIC の publisher / subscriber クライアントのサンプル。

draft-ietf-moq-transport-22、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01、draft-ietf-moq-c4m-01 に準拠。

## 構成

- **moq-pub**：映像を AV1 / H.264 / H.265 で、音声を Opus でエンコードし、video / audio / `catalog` track を MoQ relay へ PUBLISH する。`--input-mp4` を指定すると MP4 ファイルの映像トラックを再エンコードせずに配信し、`--input-mp4-reencode` を指定すると MP4 ファイルの映像 / 音声をデコードして再エンコードして配信する
- **moq-sub**：MoQ relay から catalog を FETCH し、video / audio を SUBSCRIBE してデコードして再生する。`--mp4` を指定すると受信した映像 / 音声を MP4 に保存できる。catalog の Full / Delta 適用と datagram 受信にも対応する
- **tokio-moq**：publisher / subscriber が共有する QUIC / WebTransport over HTTP/3 / WebTransport over HTTP/2 トランスポート層 (ライブラリ)

publisher / subscriber の接続先となる MoQ relay は別途用意する。

## 前提条件

Rust のバージョン前提はリポジトリルートの `README.md` を参照すること。
macOS ではカメラとマイクへのアクセス許可が必要 (H.264 / H.265 は macOS の Video Toolbox 限定)。

## ビルド

```bash
cargo build -p moq-pub -p moq-sub
```

## 実行例

```bash
# QUIC で publisher を起動 (疑似キャプチャ)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --fake-capture-device

# QUIC で subscriber を起動
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443

# WebTransport over HTTP/3 で publisher を起動
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --transport wt-h3 --fake-capture-device

# WebTransport over HTTP/2 で subscriber を起動
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --transport wt-h2

# QUIC で受信した映像 / 音声を MP4 に保存する (再生しない)
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --mp4 out.mp4 --no-play

# QUIC で MP4 ファイルの映像トラックを再エンコードせずに配信する (AV1 / H.264 / H.265)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --input-mp4 input.mp4

# QUIC で MP4 ファイルの映像 / 音声を再エンコードして配信する (音声は Opus のみ対応)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --input-mp4-reencode input.mp4
```

## トランスポート

接続経路は `--transport` で選ぶ。既定値は `quic`。

| `--transport` | 接続経路 | 備考 |
| --- | --- | --- |
| `quic` | QUIC 直接接続 | ALPN は `moqt-22` |
| `wt-h3` | WebTransport over HTTP/3 | QUIC 上の ALPN は `h3` |
| `wt-h2` | WebTransport over HTTP/2 | TCP+TLS 上の ALPN は `h2` |

WebTransport 経路では draft-ietf-webtrans-http3-16 / draft-ietf-webtrans-http2-15 に従い、Extended CONNECT でセッションを確立する。
MOQT のプロトコル識別子 (`moqt-22`) は `WT-Available-Protocols` で通知し、サーバーが `WT-Protocol` で選択しなければ接続を失敗させる (draft-ietf-moq-transport-22 §6.2)。

## C4M 認可トークン

MSF fragment (`#msf:<track-identifier>&c4m=<token>`) の `c4m` パラメータに C4M トークンを指定すると、SETUP の AUTHORIZATION_TOKEN (Token Type CAT) として送信する (draft-ietf-moq-msf-01 §11.1.1、draft-ietf-moq-c4m-01 §7.1.1)。

```bash
# C4M トークン付きで QUIC 接続する (シェルでは `&` を引用符で囲む)
cargo run -p moq-pub -- 'moqt://127.0.0.1:4443#msf:kaki--video&c4m=<base64 token>' --fake-capture-device
```

`<base64 token>` は C4M トークンのバイト列 (CBOR エンコードされた CWT) を Base64 (RFC 4648 §4) または base64url (§5) で表した文字列で、パディングは省略できる。`%XX` の percent-encoding もデコードする。トークンの検証 (署名 / クレーム / `moqt` クレームの認可判定) は relay が行い、example はデコードしたバイト列をそのまま送る。DPoP バインディング (`cnf` / `catdpop`) を使うトークンに必要な DPoP proof の送信には対応しない。

`c4m` は複数指定でき、すべてのトークンを SETUP に載せる (同一トークンは 1 つに畳む)。MSF fragment が §11.1 の ABNF に一致しない場合と、`c4m` が空または Base64 として不正な場合は `--url` の解釈でエラーになる。

example は `msf` fragment の track-identifier と `c4m` 以外のパラメータを使わない。Track Namespace と Track Name は `--namespace` / `--track-name` で指定する。

## CLI オプション

全オプションは各クレートの `--help` で確認できる。主なものを以下に挙げる。

### moq-pub

| オプション | 短縮 | デフォルト | 説明 |
| --- | --- | --- | --- |
| `--url` | `-u` | (必須) | 接続先 URL (`moqt://`) |
| `--transport` | | `quic` | 接続経路 (`quic` / `wt-h3` / `wt-h2`) |
| `--cert` | | | TLS CA 証明書パス |
| `--device-id` | | | カメラデバイス ID |
| `--width` | | `1280` | 映像幅 (px) |
| `--height` | | `720` | 映像高さ (px) |
| `--fps` | | `30` | フレームレート |
| `--bitrate` | | `2000` | 映像ターゲットビットレート (kbps) |
| `--keyframe-interval` | | `60` | キーフレーム間隔 (フレーム数) |
| `--namespace` | | `kaki` | Track Namespace |
| `--track-name` | | `video` | Track Name |
| `--fake-capture-device` | | | 実デバイスの代わりに raden 生成の疑似映像と 440 Hz サイン波音声を使う |
| `--input-mp4` | | | MP4 ファイルの映像トラックを再エンコードせずに配信する (AV1 / H.264 / H.265。音声は配信せず、B フレームを含む MP4 は拒否する。`--video-codec` / `--width` / `--height` / `--fps` / `--no-video` とは併用不可) |
| `--input-mp4-reencode` | | | MP4 ファイルの映像 / 音声をデコードして再エンコード配信する (映像は AV1 / H.264 / H.265、音声は Opus のみ。解像度 / フレームレートは MP4 から自動検出する。`--input-mp4` / `--width` / `--height` / `--fps` とは併用不可) |
| `--video-codec` | | `av1` | 映像コーデック (av1 / h264 / h265。h264 / h265 は macOS 限定) |
| `--no-video` | | | 映像トラックの送信を無効化する |
| `--no-audio` | | | 音声トラックの送信を無効化する |
| `--audio-device-id` | | | 音声入力デバイス ID |
| `--audio-bitrate` | | `64` | 音声ターゲットビットレート (kbps) |
| `--audio-datagram` | | | 音声トラックを subgroup stream ではなく datagram で配信する (映像と catalog は常に subgroup stream)。datagram は 1 object が 1 QUIC datagram に収まる必要があり (draft-ietf-moq-loc-04 §4.1)、映像は 1 group = 1 unidirectional stream で送る (同 §4.2) |

`--input-mp4` と同時に指定した場合、`--device-id` / `--fake-capture-device` / `--keyframe-interval` / `--bitrate` / `--audio-device-id` / `--audio-bitrate` / `--audio-datagram` は無視され、警告ログが出る。
`--input-mp4-reencode` と同時に指定した場合、`--device-id` / `--fake-capture-device` / `--audio-device-id` は無視され、警告ログが出る。

### moq-sub

| オプション | 短縮 | デフォルト | 説明 |
| --- | --- | --- | --- |
| `--url` | `-u` | (必須) | 接続先 URL (`moqt://`) |
| `--transport` | | `quic` | 接続経路 (`quic` / `wt-h3` / `wt-h2`) |
| `--cert` | | | TLS CA 証明書パス |
| `--namespace` | | `kaki` | Track Namespace |
| `--no-video` | | | 映像トラックの購読を無効化する |
| `--no-audio` | | | 音声トラックの購読を無効化する |
| `--audio-output-device` | | `default` | 音声の出力先。`none` はスピーカーへ出力せず受信とデコードだけを続ける (`default` と `none` のみ対応) |
| `--mp4` | | | 受信した映像 / 音声を MP4 ファイルへ保存する (再エンコードせずに mux する。既存ファイルは上書きし、datagram 経由で届いたメディアは対象外) |
| `--no-play` | | | 再生を行わない (SDL を初期化せず、デコードもしない) |

## URL スキーム

- `moqt://host[:port]/path`：接続経路は `--transport` で選ぶ

`port` を省略すると 443 を使う (draft-ietf-moq-transport-22 §6.1.2)。`host` は IP リテラル (例：`127.0.0.1` / `[2001:db8::1]`) と DNS 名の両方を受け付け、DNS 名は接続時に名前解決する。解決した接続先アドレスはログに出力される。

RFC 3986 §3.2.2 の host に一致しない authority は名前解決の前に拒否する。`[` `]` の中身は IPv6 アドレスに限るため `[example.com]` や IPvFuture (`[v1.fe80::]`)、zone id 付き IPv6 リテラル (`[fe80::1%25en0]`) はエラーになる。ポート 0 と userinfo (`user@host`) もエラーになる。エラーメッセージには拒否理由と入力した authority が含まれる。

DNS 名が複数のアドレスに解決される場合は、解決順に接続を試し、確立できたアドレスを採用する。IPv4 と IPv6 の両方に解決される `localhost` などで、先頭の接続先が待ち受けていない場合にも次のアドレスへ進む。接続の試行は `--transport` で選んだ経路の確立までを単位とし、確立後の SETUP の失敗では次のアドレスを試さない。接続先のアドレスファミリに合わせてローカルソケットを選ぶため、IPv6 の接続先にも送信できる。

query (`?key=value`) は SETUP の PATH option と WebTransport の `:path` にそのまま引き継がれる。

fragment (`#type:value`) は draft-ietf-moq-transport-22 §6.1.1 に従いサーバーへ送信せず、SETUP の PATH option と WebTransport の `:path` には含めない。§6.2.1 が WebTransport の https:// URI を moqt URI の scheme 置換と定め、§16.2 が application/moqt の fragment identifier を同節に従わせるため、WebTransport 経路でも同じ規則で扱う。
`type` は ASCII 小文字 / 数字 / ハイフンに限り、`:` を含まない fragment、空または規則に一致しない `type`、fragment 内の 2 個目の `#` (RFC 3986 §3.5 により `%23` が必要) はエラーになる。

example は `type` が `msf` のときだけ値を MSF fragment (`track-identifier [ "&" parameter-list ]`) として検証し、予約パラメータ `c4m` の認可トークンを取り出す (前述の「C4M 認可トークン」)。`msf` 以外の `type` の値は構文 (§6.1.1) の検証だけを行い、解釈しない。

scheme は RFC 3986 §3.1 に従い大文字小文字を区別しない (`MOQT://` も受理する)。`moqt://` 以外の scheme はエラーになる。authority / path / query / fragment は入力の大文字小文字をそのまま使う。

## TLS 証明書

publisher / subscriber は `--cert` を省略すると証明書検証をスキップする。

本番環境では `--cert` で CA 証明書を指定して証明書検証を有効にすること。

## ログ

環境変数 `RUST_LOG` でログレベルを制御できる。

```bash
RUST_LOG=debug cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --fake-capture-device
```
