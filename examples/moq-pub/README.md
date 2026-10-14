# moq-pub

MoQ リレーへ catalog / audio / video track を PUBLISH する publisher クライアントのサンプル。

音声は Opus / AAC-LC で、映像は VP8 / VP9 / AV1 / H.264 / H.265 でエンコードする。

AAC-LC は macOS の Audio Toolbox、H.264 / H.265 は macOS の Video Toolbox を使うため macOS 限定である。
VP8 / VP9 は libvpx を使う。libvpx の Windows ビルド (MSVC) は動作しないため、Windows では `vp8` / `vp9` を指定できない。

入力はカメラとマイクのほかに MP4 ファイルに対応し、`--input-mp4` は映像トラックを再エンコードせずに配信し、`--input-mp4-reencode` は音声 / 映像をデコードして再エンコードして配信する。

publisher / subscriber の接続先となる MoQ リレーは別途用意する。
`--url` の URL スキーム、`--transport`、`--namespace`、C4M 認可トークン、TLS 証明書の指定は publisher と subscriber で共通である。規則は [tokio-moq](../tokio-moq/README.md) を参照すること。

## 実行例

```bash
# QUIC で起動する (440 Hz サイン波音声と raden 生成の疑似映像)
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --fake-capture-device

# WebTransport over HTTP/3 で起動する
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --transport wt-h3 --fake-capture-device

# MP4 ファイルの映像トラックを再エンコードせずに配信する
# 対応する映像コーデックは AV1 / H.264 / H.265
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --input-mp4 input.mp4

# MP4 ファイルの音声 / 映像を再エンコードして配信する
# 音声は Opus のみ対応する
cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --input-mp4-reencode input.mp4

# MSF fragment から Track Namespace を取る (`--namespace` を省略)
cargo run -p moq-pub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video' --fake-capture-device
```

## CLI オプション

全オプションは `cargo run -p moq-pub -- --help` で確認できる。主なものを以下に挙げる。

| オプション | デフォルト | 説明 |
| --- | --- | --- |
| `--url` | (必須) | 接続先 URL (`moqt://`) |
| `--transport` | (省略時は `--url` の `msf` fragment の `connection`、無ければ `quic`) | 接続経路 (`quic` / `wt-h3` / `wt-h2`) |
| `--cert` | | TLS CA 証明書パス |
| `--device-id` | | カメラデバイス ID |
| `--width` | `1280` | 映像幅 (px) |
| `--height` | `720` | 映像高さ (px) |
| `--fps` | `30` | フレームレート |
| `--bitrate` | `2000` | 映像ターゲットビットレート (kbps) |
| `--gop-duration` | `2000` | 映像の Group 長 (ms)。入力フレームの Timestamp の差分がこの値に達したらキーフレームを強制し、新しい Group を開始する。Group 長は境界のフレームやカデンスのずれで 1 フレーム前後する。カタログの `maxGopDuration` / `maxGroupDuration` にはこの値 + 1 フレーム分を載せ、入力のゆらぎ・フレーム落ち・エンコーダ出力の遅延があると実測はこれを超え得る |
| `--namespace` | (省略時は `--url` の `msf` fragment) | Track Namespace (§8.8 表現。`-` が namespace フィールドの区切り) |
| `--track-name` | `video` | Track Name |
| `--fake-capture-device` | | 実デバイスの代わりに 440 Hz サイン波音声と raden 生成の疑似映像を使う |
| `--input-mp4` | | MP4 ファイルの映像トラックを再エンコードせずに配信する (AV1 / H.264 / H.265。音声は配信せず、B フレームを含む MP4 は拒否する) |
| `--input-mp4-reencode` | | MP4 ファイルの音声 / 映像をデコードして再エンコード配信する (音声は Opus のみ、映像は AV1 / H.264 / H.265。解像度 / フレームレートは MP4 から自動検出する) |
| `--video-codec` | `av1` | 映像コーデック (vp8 / vp9 / av1 / h264 / h265。h264 / h265 は macOS 限定、vp8 / vp9 は Windows 非対応) |
| `--no-video` | | 映像トラックの送信を無効化する |
| `--no-audio` | | 音声トラックの送信を無効化する |
| `--audio-device-id` | | 音声入力デバイス ID |
| `--audio-codec` | `opus` | 音声コーデック (opus / aac。aac は macOS 限定で AAC-LC のみ) |
| `--audio-bitrate` | `64` | 音声ターゲットビットレート (kbps) |
| `--audio-datagram` | | 音声トラックを subgroup stream ではなく datagram で配信する (catalog と映像は常に subgroup stream)。datagram は 1 object が 1 QUIC datagram に収まる必要があり (draft-ietf-moq-loc-04 §4.1)、映像は 1 group = 1 unidirectional stream で送る (同 §4.2)。上限は `--datagram-max-size` で指定し、超える object はエラーになる |
| `--datagram-max-size` | `1160` | MOQT の object datagram (ヘッダ + Properties + payload) の上限サイズ (bytes)。上限を超える object は送信せず、実サイズと対処を示すエラーで終了する。既定は RFC 9000 §14 が定める経路の最小 datagram サイズ 1200 から QUIC のパケットヘッダと DATAGRAM frame のヘッダ分を引いた保守的な値である |
| `--target-latency` | `200` | MSF カタログの `targetLatency` (ms)。受信側は符号化時刻からこの値ぶん遅らせて表示する (draft-ietf-moq-msf-01 §5.2.8)。音声と映像には同じ値を載せ、同じ `renderGroup` で同時に再生させる (同 §5.2.11) |

## 注意事項

`--input-mp4` と `--input-mp4-reencode` には同時指定の制約がある。併用できないオプションを指定した場合はエラーになり、無視されるオプションを指定した場合は警告ログが出る。

- `--input-mp4` と併用できないオプション
  - `--video-codec`
  - `--width`
  - `--height`
  - `--fps`
  - `--no-video`
- `--input-mp4-reencode` と併用できないオプション
  - `--input-mp4`
  - `--width`
  - `--height`
  - `--fps`
- `--input-mp4` と同時に指定すると無視されるオプション
  - `--device-id`
  - `--fake-capture-device`
  - `--gop-duration`
    - カタログの `maxGopDuration` / `maxGroupDuration` にも載せない
  - `--bitrate`
  - `--audio-device-id`
  - `--audio-bitrate`
  - `--audio-datagram`
  - `--datagram-max-size`
- `--input-mp4-reencode` と同時に指定すると無視されるオプション
  - `--device-id`
  - `--fake-capture-device`
  - `--audio-device-id`

## 購読者からの新しい Group の要求

映像の Group はキーフレームから始まる。そのため Group の途中で視聴を始めた購読者は、次のキーフレームが届くまで映像を表示できない。`--gop-duration` を長くするほど、この待ち時間も長くなる。

購読者は購読を始めるときに、次の Group をできるだけ早く始めるよう要求できる。この要求が `NEW_GROUP_REQUEST` である。moq-pub は要求に応じて `--gop-duration` の境界を待たずにキーフレームを作り、新しい Group を始める。Group の先頭はキーフレームなので、購読者は待たずに映像を見始められる。

moq-pub はこの要求を次のように扱う。

- 映像トラックの PUBLISH と SUBSCRIBE_OK で `DYNAMIC_GROUPS=1` を告知する。これは Group を動的に始められるという意味で、購読者はこれを見て購読中でも REQUEST_UPDATE で要求を送れる
- 要求は SUBSCRIBE と REQUEST_UPDATE のどちらに載っていても受け付ける
- 受け付けるのは映像トラックの購読だけである。値が 0 か、現在の Group より大きい値のときだけ受け付ける。まだ Group を始めていない間は値にかかわらず受け付ける。最初の Group は最初のキーフレームで始まるため、要求はそこで満たされる
- 直前の Group が 300 ms に達するまでは前倒ししない。300 ms に達した後の最初の入力フレームでキーフレームを作る。`--gop-duration` による通常の境界が先に来れば、そこで Group が始まる
- Group ID は publisher が決めるため、要求された値と一致させる必要はない
- `--input-mp4` のパススルー配信はキーフレームを作れないため、`DYNAMIC_GROUPS` を告知せず、要求も受け付けない

仕様上の扱いは draft-ietf-moq-transport-22 §9.20.19 (NEW_GROUP_REQUEST)、§10.6 (DYNAMIC_GROUPS)、§3.5.1 (Dynamically Starting New Groups) を参照すること。

## ログ

環境変数 `RUST_LOG` でログレベルを制御できる。未設定の場合の既定は `error` である。WebTransport が experimental である旨などの警告ログを見るには `RUST_LOG=warn` 以上を指定する。

```bash
RUST_LOG=debug cargo run -p moq-pub -- --url moqt://127.0.0.1:4443 --namespace moq-example --fake-capture-device
```
