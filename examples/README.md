# MoQT サンプル

MoQT (Media over QUIC Transport) の publisher / subscriber クライアントのサンプル。

draft-ietf-moq-transport-21、draft-ietf-moq-loc-04、draft-ietf-moq-msf-01 に準拠。

## 構成

- **moqt-publisher**：映像を AV1 / H.264 / H.265 で、音声を Opus でエンコードし、video / audio / `catalog` track を MoQT relay へ PUBLISH する
- **moqt-subscriber**：MoQT relay から catalog を FETCH し、video / audio を SUBSCRIBE してデコードして再生する。catalog の Full / Delta 適用と datagram 受信にも対応する
- **moqt-transport**：publisher / subscriber が共有する QUIC / WebTransport over HTTP/3 トランスポート層 (crate 名 `moqt-example-transport`、ライブラリ)

publisher / subscriber の接続先となる MoQT relay は別途用意する。

## 前提条件

Rust のバージョン前提はリポジトリルートの `README.md` を参照すること。
macOS ではカメラとマイクへのアクセス許可が必要 (H.264 / H.265 は macOS の Video Toolbox 限定)。

## ビルド

```bash
cargo build -p moqt-publisher -p moqt-subscriber
```

## 実行例

```bash
# QUIC で publisher を起動 (疑似キャプチャ)
cargo run -p moqt-publisher -- --url moqt://127.0.0.1:4443 --fake-capture-device

# QUIC で subscriber を起動
cargo run -p moqt-subscriber -- --url moqt://127.0.0.1:4443
```

`--url https://host[:port]/path` を指定すると WebTransport over HTTP/3 で接続する。

## CLI オプション

全オプションは各クレートの `--help` で確認できる。主なものを以下に挙げる。

### moqt-publisher

| オプション | 短縮 | デフォルト | 説明 |
| --- | --- | --- | --- |
| `--url` | `-u` | (必須) | 接続先 URL (moqt:// または https://) |
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
| `--video-codec` | | `av1` | 映像コーデック (av1 / h264 / h265。h264 / h265 は macOS 限定) |
| `--no-video` | | | 映像トラックの送信を無効化する |
| `--no-audio` | | | 音声トラックの送信を無効化する |
| `--audio-device-id` | | | 音声入力デバイス ID |
| `--audio-bitrate` | | `64` | 音声ターゲットビットレート (kbps) |
| `--use-datagram` | | | subgroup stream ではなく datagram で映像 / 音声オブジェクトを配信する (catalog は常に subgroup stream) |

### moqt-subscriber

| オプション | 短縮 | デフォルト | 説明 |
| --- | --- | --- | --- |
| `--url` | `-u` | (必須) | 接続先 URL (moqt:// または https://) |
| `--cert` | | | TLS CA 証明書パス |
| `--namespace` | | `kaki` | Track Namespace |
| `--no-video` | | | 映像トラックの購読を無効化する |
| `--no-audio` | | | 音声トラックの購読を無効化する |
| `--audio-output-device` | | `default` | 音声の出力先。`none` はスピーカーへ出力せず受信とデコードだけを続ける (`default` と `none` のみ対応) |

## URL スキーム

- `moqt://host[:port]/path`：QUIC 直接接続
- `https://host[:port]/path`：WebTransport over HTTP/3

`port` を省略すると 443 を使う (draft-ietf-moq-transport-21 §6.1.2)。`host` は IP リテラル (例：`127.0.0.1` / `[2001:db8::1]`) と DNS 名の両方を受け付け、DNS 名は接続時に名前解決する。解決した接続先アドレスはログに出力される。

DNS 名が複数のアドレスに解決される場合は、先頭のアドレスに接続する。接続先のアドレスファミリに合わせてローカルソケットを選ぶため、IPv6 の接続先にも送信できる。

query (`?key=value`) は SETUP の PATH option と WebTransport の `:path` にそのまま引き継がれる。

fragment (`#type:value`) は draft-ietf-moq-transport-21 §6.1.1 に従いサーバーへ送信せず、SETUP の PATH option と WebTransport の `:path` には含めない。§6.2.1 が WebTransport の https:// URI を moqt URI の scheme 置換と定め、§16.2 が application/moqt の fragment identifier を同節に従わせるため、moqt:// と https:// のどちらも同じ規則で扱う。
`type` は ASCII 小文字 / 数字 / ハイフンに限り、`:` を含まない fragment、空または規則に一致しない `type`、fragment 内の 2 個目の `#` (RFC 3986 §3.5 により `%23` が必要) はエラーになる。example は fragment の値を解釈しない。

scheme は RFC 3986 §3.1 に従い大文字小文字を区別しない (`MOQT://` / `HTTPS://` も受理する)。authority / path / query / fragment は入力の大文字小文字をそのまま使う。

## TLS 証明書

publisher / subscriber は `--cert` を省略すると証明書検証をスキップする。

本番環境では `--cert` で CA 証明書を指定して証明書検証を有効にすること。

## ログ

環境変数 `RUST_LOG` でログレベルを制御できる。

```bash
RUST_LOG=debug cargo run -p moqt-publisher -- --url moqt://127.0.0.1:4443 --fake-capture-device
```
