# moq-sub

MoQ リレーから catalog を SUBSCRIBE して取得し、audio / video を SUBSCRIBE してデコードして再生する subscriber クライアントのサンプル。

購読より前に publish された直近の Group は、購読後に FETCH で取得する。

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

# QMux (TCP+TLS。UDP が使えない環境向け。experimental) で起動する
cargo run -p moq-sub -- --url moqt://127.0.0.1:4443 --namespace moq-example --transport qmux

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
| `--transport` | (省略時は `--url` の `msf` fragment の `connection`、無ければ `quic`) | 接続経路 (`quic` / `wt-h3` / `wt-h2` / `qmux`)。`wt-h3` / `wt-h2` / `qmux` は experimental |
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

WebTransport 経路 (`--transport wt-h3` / `--transport wt-h2`) は experimental であり、正常に動作しない。
`wt-h3` は s2n-quic が reset stream (`RESET_STREAM_AT`) を実装していないため、WebTransport が MUST とする Reliable Size 付きのデータストリーム reset を行えない。`reset_stream_at` を要求する MoQ リレーとは接続できず、reset を伴う経路 (request stream の cancel や、送信中の subgroup / data stream の中断) も成立しない。
`wt-h2` は reset を `WT_RESET_STREAM` capsule で行うため `RESET_STREAM_AT` は不要だが、MoQ リレーの capsule 対応に依存しており、実リレーでの動作を確認していない。
詳細は [tokio-moq](../tokio-moq/README.md) を参照すること。

QMux 経路 (`--transport qmux`) は experimental である。QMux は TCP + TLS の上で QUIC version 1 のストリームと DATAGRAM を多重化する経路で、UDP が使えない環境向けである。
音声の datagram は経路での損失を下位の TCP が再送するため起きないが、TCP の再送で遅延が伸びる。QMux は速やかに配送できない DATAGRAM の破棄を許すため、受信側のバッファが溢れると欠落しうる。詳細は [tokio-moq](../tokio-moq/README.md) を参照すること。

## 再生と遅延

受信した音声 / 映像は catalog の情報を使って再生する。catalog の `targetLatency` は表示の遅れの下限として使う (draft-ietf-moq-msf-01 §5.2.8)。

- 音声：復号した音声をその場で出力のキューへ積み、最初の音の目標時刻に達してから再生を開始する。到着の揺らぎは目標遅延ぶんの先行バッファで吸収する
- 音声の隙間：鳴らす時刻の差として生じた隙間は、直前の音の周期による補間か、フェード付きの無音で材料化する（積んだ順に鳴らす出力が、隙間の分だけ早く鳴りすぎないようにする）
- 映像：共通の時間軸が決めた表示時刻に合わせて表示する

## 購読者からの新しい Group の要求

映像の Group はキーフレームから始まる。そのため Group の途中で購読を始めると、次のキーフレームが届くまで映像を表示できない。

moq-sub は映像トラックの SUBSCRIBE に `NEW_GROUP_REQUEST=0` を載せる。これは「Group の情報を持たないので、次の Group をできるだけ早く始めてほしい」という要求である。moq-pub はこれに応じて Group の開始を前倒しするため、Group の途中で参加しても次の周期キーフレームを待たずに映像を見始められる。

購読中でも REQUEST_UPDATE で要求を送れるが、送れるのは相手が `DYNAMIC_GROUPS=1` を告知している Track だけである。moq-pub は映像トラックの PUBLISH と SUBSCRIBE_OK の両方で告知するため、購読の確立経路によらず送れる。

仕様上の扱いは draft-ietf-moq-transport-22 §9.20.19 (NEW_GROUP_REQUEST) と §10.6 (DYNAMIC_GROUPS) を参照すること。

## 購読より前の Group の取得 (FETCH)

購読を始めた時点より前に publish された Object は SUBSCRIBE では届かないが、音声の購読には Location Filter を載せていないため、購読が受け取る範囲は Largest Object の位置を含みうる。moq-sub は映像と音声を購読したあと、SUBSCRIBE_OK が告知した Largest Object (draft-ietf-moq-transport-22 §3.1.4 / §9.20.17) を基準に直近 4 Group を FETCH で要求する (§3.2 / §3.5.1)。

- 要求 range は LOCATION_FILTER の Absolute Range (§9.20.9 Table 6 の Type 0x04) で、`{Largest Object の Group から 4 Group 前, 0}` から Largest Object までを指定する
- 取得した Object は購読で届く Object と同じ経路 (録画 → デコード → 表示) へ流す (音声は下の境界の規則で選別する)。映像はキーフレームから復号を始めるため、すでに復号中の Group より古い Group の Object は表示しない。録画は live の最初のキーフレームを基準時刻にするため、live の録画が先に始まっていると FETCH で遡った Group は基準時刻より前として破棄される (録画に残るのは live より先に FETCH の Object が届いた場合だけ)
- 音声は `SUBSCRIBE_OK` の Largest Object を購読の境界にする。境界以下の音声は購読より前に publish された分であり、Audio Config の検証だけを行い、送出も録画も復号もしない (境界の位置の Object は live と FETCH の両方で届きうるため、二重送出を防ぐ)。境界より後の live の音声は TIMESTAMP の順で送出し、並び替わって届いた古い音声は送出しない (draft-ietf-moq-transport-22 §2.1.2 / §2.3.1)
- 応答の End of Range (Non-Existent / Unknown / Timed-Out) は「取得できなかった範囲」として info ログに残す (§11.4.1.2)
- SUBSCRIBE_OK が Largest Object を運ばない場合と、FETCH を発行できなかった場合はログを残し、購読はそのまま続ける (example は停止しない)
- Largest Object が告知されない publisher と、メディアの FETCH に応答しない publisher では購読より前の Group を取得できない

メディアの fill fetch (§3.4) は使わない。

## datagram 配送の音声

音声トラックは subgroup stream と datagram の両方で届きうる。LOC は音声について「1 audio chunk = 1 Object = 1 Group」の datagram 配送を例示しており (draft-ietf-moq-loc-04 §4.1)、moq-pub は `--audio-datagram` でこの配送を選べる。

moq-sub は datagram で届いた OBJECT_DATAGRAM を自ら decode し、subgroup stream と同じ経路 (録画 → Audio Config の検証 → デコード → 音声の再生の組み立て) へ流す。

- フィルタを通過しなかった Object とキャンセル済み subscription の Object、Object Status のみの Object は再生しない
- 映像とカタログの datagram には対応しない (LOC の映像は 1 group = 1 unidirectional stream で配送される。同 §4.2)。届いた場合はトラックごとに警告を 1 回出す
- LOC Properties の書式違反は subgroup stream と同じくセッション終了の対象になる (draft-ietf-moq-transport-22 §8.3)

仕様上の扱いは draft-ietf-moq-transport-22 §2.1 (Objects) と §11.2.1 (Object Datagram) を参照すること。

## ログ

環境変数 `RUST_LOG` でログレベルを制御できる。未設定の場合の既定は `info` である。
