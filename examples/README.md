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

`--url https://host:port/path` を指定すると WebTransport over HTTP/3 で接続する。

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

## URL スキーム

- `moqt://host:port/path`：QUIC 直接接続
- `https://host:port/path`：WebTransport over HTTP/3

現状のサンプルは `host` に IP リテラル (例：`127.0.0.1`) のみを受け付ける。DNS 名は名前解決を行わないため接続できない。

query (`?key=value`) は SETUP の PATH option と WebTransport の `:path` にそのまま引き継がれる。

## TLS 証明書

publisher / subscriber は `--cert` を省略すると証明書検証をスキップする。

本番環境では `--cert` で CA 証明書を指定して証明書検証を有効にすること。

## ログ

環境変数 `RUST_LOG` でログレベルを制御できる。

```bash
RUST_LOG=debug cargo run -p moqt-publisher -- --url moqt://127.0.0.1:4443 --fake-capture-device
```
