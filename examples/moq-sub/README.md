# moq-sub

MoQ リレーから catalog を SUBSCRIBE して取得し、audio / video を SUBSCRIBE してデコードして再生する subscriber クライアントのサンプル。

音声は Opus / AAC-LC、映像は VP8 / VP9 / AV1 / H.264 / H.265 に対応する。

音声トラックは subgroup stream と datagram のどちらで届いてもデコードして再生する。

AAC-LC は macOS の Audio Toolbox、H.264 / H.265 は macOS の Video Toolbox を使うため macOS 限定である。
VP8 / VP9 は libvpx を使う。libvpx の Windows ビルド (MSVC) は動作しないため、Windows では `vp8` / `vp09.*` のトラックをデコードできない。

`--mp4` を指定すると、受信した音声 / 映像を再エンコードせずに MP4 ファイルへ保存する。

publisher / subscriber の接続先となる MoQ リレーは別途用意する。
`--url` の URL スキーム、`--transport`、`--namespace`、C4M 認可トークン、TLS 証明書の指定は publisher と subscriber で共通である。規則は [tokio-moq](../tokio-moq/README.md) を参照すること。

## 実行例

```bash
# QUIC で起動する
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example

# WebTransport over HTTP/2 で起動する
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example --transport wt-h2

# 受信した音声 / 映像を MP4 に保存する (再生しない)
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example --mp4 out.mp4 --no-play

# 音声を出力せずに受信とデコードだけを続ける
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example --audio-output-device none

# MSF fragment から Track Namespace を取る (`--namespace` を省略)
cargo run -p moq-sub -- --url 'moqt://127.0.0.1:4443#msf:moq-example--video'
```

## CLI オプション

全オプションは `cargo run -p moq-sub -- --help` で確認できる。主なものを以下に挙げる。

| オプション | デフォルト | 説明 |
| --- | --- | --- |
| `--url` | (必須) | 接続先 URL (`moqt://`) |
| `--transport` | (省略時は `--url` の `msf` fragment の `connection`、無ければ `quic`) | 接続経路 (`quic` / `wt-h3` / `wt-h2`) |
| `--cert` | | TLS CA 証明書パス |
| `--namespace` | (省略時は `--url` の `msf` fragment) | Track Namespace (§8.8 表現。`-` が namespace フィールドの区切り) |
| `--no-video` | | 映像トラックの購読を無効化する |
| `--no-audio` | | 音声トラックの購読を無効化する |
| `--audio-output-device` | `default` | 音声の出力先。`none` はスピーカーへ出力せず受信とデコードだけを続ける (`default` と `none` のみ対応) |
| `--mp4` | | 受信した音声 / 映像を MP4 ファイルへ保存する (再エンコードせずに mux する。音声は Opus のみで、AAC-LC のトラックでは音声を録画せず警告する。既存ファイルは上書きし、音声は subgroup stream と datagram のどちらで届いても保存する) |
| `--no-play` | | 再生を行わない (SDL を初期化せず、デコードもしない) |

## 注意事項

- `--no-play` と `--mp4` を併用した場合はデコードをスキップし、MP4 への保存だけを行う
- `--no-play` を `--mp4` なしで指定した場合は受信したメディアを破棄する (警告ログが出る)

## 再生と遅延

受信した音声 / 映像は catalog の情報を使って再生する。catalog の `targetLatency` は表示の遅れの下限として使う (draft-ietf-moq-msf-01 §5.2.8)。

- 音声：jitter buffer が到着の揺らぎに合わせた遅延までデコード済みの音声を保持してから鳴らす
- 映像：共通の時間軸が決めた表示時刻に合わせて表示する

## 購読者からの新しい Group の要求

映像の Group はキーフレームから始まる。そのため Group の途中で購読を始めると、次のキーフレームが届くまで映像を表示できない。

moq-sub は映像トラックの SUBSCRIBE に `NEW_GROUP_REQUEST=0` を載せる。これは「Group の情報を持たないので、次の Group をできるだけ早く始めてほしい」という要求である。moq-pub はこれに応じて Group の開始を前倒しするため、Group の途中で参加しても次の周期キーフレームを待たずに映像を見始められる。

購読中でも REQUEST_UPDATE で要求を送れるが、送れるのは相手が `DYNAMIC_GROUPS=1` を告知している Track だけである。moq-pub は映像トラックの PUBLISH と SUBSCRIBE_OK の両方で告知するため、購読の確立経路によらず送れる。

仕様上の扱いは draft-ietf-moq-transport-22 §9.20.19 (NEW_GROUP_REQUEST) と §10.6 (DYNAMIC_GROUPS) を参照すること。

## datagram 配送の音声

音声トラックは subgroup stream と datagram の両方で届きうる。LOC は音声について「1 audio chunk = 1 Object = 1 Group」の datagram 配送を例示しており (draft-ietf-moq-loc-04 §4.1)、moq-pub は `--audio-datagram` でこの配送を選べる。

moq-sub は datagram で届いた OBJECT_DATAGRAM を自ら decode し、subgroup stream と同じ経路 (録画 → Audio Config の検証 → デコード → jitter buffer) へ流す。

- フィルタを通過しなかった Object とキャンセル済み subscription の Object、Object Status のみの Object は再生しない
- 映像とカタログの datagram には対応しない (LOC の映像は 1 group = 1 unidirectional stream で配送される。同 §4.2)。届いた場合はトラックごとに警告を 1 回出す
- LOC Properties の書式違反は subgroup stream と同じくセッション終了の対象になる (draft-ietf-moq-transport-22 §8.3)

仕様上の扱いは draft-ietf-moq-transport-22 §2.1 (Objects) と §11.2.1 (Object Datagram) を参照すること。

## ログ

環境変数 `RUST_LOG` でログレベルを制御できる。未設定の場合の既定は `info` である。
