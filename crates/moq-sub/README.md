# moq-sub

MoQ リレーから catalog を SUBSCRIBE して取得し、audio / video を SUBSCRIBE してデコードして再生する subscriber クライアントのサンプル。

購読より前に publish された直近の Group は、映像だけ購読後に FETCH で取得する。

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
- 映像：共通の時間軸が決めた表示時刻を目標に、表示待ちのキューから目標に最も近い 1 枚を選んで表示する。表示できるフレームが無いときだけ 20 ms を上限に遅れて提示する (逃げ道)。描画の実績は再生機器 (`raw_player`) の統計から観測し、表示待ちのフレーム数・投入数・描画数・破棄数を分けてログに出す。選択で提示しなかったフレーム (遅れ、同じ表示周期に重なった分) と、再生機器が破棄したフレーム (キュー上限超過、遅延、復帰時) を分けて数える。描画を観測した時刻は、推定した表示時刻ではなく観測した時刻のまま時間軸へ記録する。
- 購読の境界以下の映像は、再生する場合は復号はするが表示待ちへは積まない (後述の「購読より前の Group の取得 (FETCH)」を参照)
- Group の切り替え：前の Group の Subgroup stream が開いている間は、次の Group の映像 Object を最大 50 ms 保留してから復号する。次の Group のキーフレームを先に復号すると、遅れて届いた前の Group の末尾が古い Group として捨てられ、GOP の末尾が 1〜数枚欠けて 33〜100 ms の停止として見えるためである。
  保留は前の Group の stream の終端 (FIN / RESET_STREAM / STOP_SENDING / 読み取りエラー) か、50 ms の期限で解除する。targetLatency が既定の 200 ms 程度であれば、この 50 ms は表示の遅れの下限の範囲に収まる。FIN の遅延が 50 ms を超える場合は保留で覆えず、末尾の欠落が残る。
- 映像の表示周期：選択を呼ぶ間隔から表示周期を実測し、目標表示時刻・許容幅・遅れの上限を決める。目標は「今 + 1 周期」、許容幅は「周期 / 2」、遅れの上限は「max(20 ms, 周期) + 1 周期」である。vsync は無効にしているため、表示の量子化はループ周期 (1 ms ループで、実測は 1〜3 ms と推定) になる
- 映像の復帰：`raw_player` は映像のみ再生で描画できない状態が 1 秒続くと最新フレームへ復帰する。moq-sub も 1 秒以上描画が進まず、その間に 2 枚以上を投入している場合は、映像キューと再生時計の基準を捨てて復帰する (古い `raw_player` 向けの保険)。復帰回数はログの `drain_recoveries` と `player_resyncs` に出る

## 途中参加の手段

映像の Group はキーフレームから始まる。そのため Group の途中で購読を始めると、次のキーフレームが届くまで映像を表示できない。

途中参加の既定の手段は `NEW_GROUP_REQUEST` であり、次の Group の Location Filter と FILL_PARAMETERS 付き SUBSCRIBE は使わない。

moq-sub は映像トラックの SUBSCRIBE に `NEW_GROUP_REQUEST=0` を載せる (draft-ietf-moq-transport-23 §3.5.1 (Dynamically Starting New Groups) / §9.19.19)。これは「Group の情報を持たないので、次の Group をできるだけ早く始めてほしい」という要求であり、購読者は LARGEST_OBJECT を知る必要が無く、cache 無効の relay でも動く。moq-pub はこれに応じて Group の開始を前倒しするため、Group の途中で参加しても次の周期キーフレームを待たずに映像を見始められる。

購読中でも REQUEST_UPDATE で要求を送れるが、送れるのは相手が `DYNAMIC_GROUPS=1` を告知している Track だけである。moq-pub は映像トラックの PUBLISH と SUBSCRIBE_OK の両方で告知するため、購読の確立経路によらず送れる。

次 Group の Location Filter (`LocationFilter::RelativeGroup { start_group: 0 }`、Type 0x01 Relative Start の StartGroup=0) は使わない。cache 有効時の起動は relay の replay が決めており、フィルタを追加すると replay を捨ててしまうためである。

FILL_PARAMETERS 付き SUBSCRIBE (§3.5 の「現在 Group から参加」) も採用しない。複数 Object の fill range を返す writer が未整備であり、moq-sub のメディア fill stream の配送と moq-pub のメディア fill 応答を実装する必要がある。さらに実測に使った relay は cache だけを対象にした fill range しか配らず、cache 無効の配備では使えない。将来必要になった場合は別途設計する。

仕様上の扱いは draft-ietf-moq-transport-23 §9.19.19 (NEW_GROUP_REQUEST) と §10.6 (DYNAMIC_GROUPS) を参照すること。

## 購読より前の Group の扱い (FETCH と relay の replay)

購読を始めた時点より前に publish された Object は SUBSCRIBE では届かない。moq-sub は映像を購読したあと、SUBSCRIBE_OK が告知した Largest Object (draft-ietf-moq-transport-23 §3.1.4 / §9.19.17) を基準に直近 4 Group (Largest Object の Group を含む) を FETCH で要求し (§3.2 / §3.5.1)、映像はその範囲のキーフレームから復号を始められるようにする。FETCH を発行するのは映像だけで、音声の明示 FETCH は発行しない (下記)。

- 要求 range は LOCATION_FILTER の Absolute Range (§9.19.9 Table 6 の Type 0x04) で、`{Largest Object の Group を含む直近 4 Group の先頭, 0}` から Largest Object までを指定する
- 取得した Object は購読で届く Object と同じ経路 (録画 → デコード → 表示) へ流す。起動時の購読では FETCH の range の終端が購読の境界 (SUBSCRIBE_OK の Largest Object) であるため、取得した Object はすべて境界以下になり表示には寄与しない
- 再購読では境界を最後に受信した位置にするため、FETCH の Object が境界より後になることがあり、復号順の判定が先に進んでいなければ表示されうる。録画は live の最初のキーフレームを基準時刻にするため、FETCH の Object が live より先に届いてキーフレームと VIDEO_CONFIG が揃えば録画の基準を早めうる。live の録画が先に始まっていれば、FETCH で遡った Group は基準時刻より前として破棄される
- 映像は `SUBSCRIBE_OK` の Largest Object を購読の境界にする。境界以下 (境界と同じ位置を含む) の映像は購読より前に publish された分であり、再生する場合は復号はするが表示しない。relay が cache から再送する購読前の Object (replay) と、FETCH で遡った Group の Object がこれに当たる
- 境界以降のフレームが境界以下のフレームを参照しうるため、再生する場合は復号を落とさない (moqt-js の CatchUpGate と同じ扱い)。取得した Group が復号中の Group より古い場合は、復号順の判定により復号しない。再生しない指定と、表示待ち超過で復号器を捨てた場合は復号もしない
- 録画は境界の判定より前に行うため、replay が運ぶキーフレームは従来どおり録画の基準になり、録画の先頭には購読前のフレームが残りうる。境界の位置の Object は FETCH の応答に含まれうるほか、replay が同じ位置から再送することもあるが、どちらも境界以下として落ちるため二重に表示されない
- 音声は `SUBSCRIBE_OK` の Largest Object を購読の境界にする。境界以下の音声は購読より前に publish された分であり、Audio Config の検証だけを行い、送出も録画も復号もしない (境界の位置の Object は live と replay の両方で届きうるため、二重送出を防ぐ)。境界より後の live の音声は TIMESTAMP の順で送出し、並び替わって届いた古い音声は送出しない (draft-ietf-moq-transport-23 §2.2 (Subgroups) / §2.3.1 (Group IDs))
- 音声の明示 FETCH は発行しない。実 relay での計測 (起動時・`node.drain_sessions` による session 移行・request stream 単位の再発行の 3 経路) で、音声 FETCH が届ける Object はすべて購読境界で落ちるか、購読側がすでに受け取っており、起動時間・再生・録画のいずれにも寄与しなかった
  - 起動時は `SUBSCRIBE_OK` の Largest Object が境界になるため、FETCH で遡る範囲の Object はすべて境界以下になる
  - session 移行では、relay が GOAWAY の Timeout まで購読を維持して配送を続け、切替時の境界は「切り替え直前に読み直した最後の受信位置」になる。FETCH の range の終端は再購読時の Largest Object であり、移行に時間がかかるほど「最後の受信位置」より前になるため、FETCH の Object はすべて境界以下になる
  - request stream 単位の再発行では、旧購読を閉じてから再購読するまでの音声も途切れなかった (この relay は `STOP_SENDING` の後も購読を維持して配送を続ける)
  - 音声の FETCH 応答経路 (専用の stream 読み取りと配送先の振り分け) も持たない。残る OpusHead の取得は再生に必須でなく、録画は catalog の値で代替できる
  - 音声の購読境界の規則は relay の replay への防御として残す (将来 fill を入れる場合は、fill range が Largest Object を超えず (§3.4)、起動時の購読では fill で届く Object がすべて境界以下になるため、境界の規則を同時に見直す必要がある)
- GOAWAY または transport drain による再購読では、境界を新しい `SUBSCRIBE_OK` の Largest Object にしない。移行中・再発行中に publish された Object を境界以下として捨ててしまうためである。session 移行では切り替え直前に読み直した最後の受信位置、request stream 単位の再発行では再発行前の最後の受信位置を境界にする。映像の境界は後退させない (replay の受信途中でも購読前の Object が表示経路へ戻らないようにするため)
- 応答の End of Range (Non-Existent / Unknown / Timed-Out / Gone) は「取得できなかった範囲」として info ログに残す (draft-ietf-moq-transport-23 §11.4.2 (End of Range))
- SUBSCRIBE_OK が Largest Object を運ばない場合と、FETCH を発行できなかった場合はログを残し、購読はそのまま続ける (example は停止しない)
- Largest Object が告知されない publisher と、映像の FETCH に応答しない publisher では購読より前の Group を取得できない

メディアの fill fetch (§3.4) は使わない。

## datagram 配送の音声

音声トラックは subgroup stream と datagram の両方で届きうる。LOC は音声について「1 audio chunk = 1 Object = 1 Group」の datagram 配送を例示しており (draft-ietf-moq-loc-04 §4.1)、moq-pub は `--audio-datagram` でこの配送を選べる。

moq-sub は datagram で届いた OBJECT_DATAGRAM を自ら decode し、subgroup stream と同じ経路 (録画 → Audio Config の検証 → デコード → 音声の再生の組み立て) へ流す。

- フィルタを通過しなかった Object とキャンセル済み subscription の Object、Object Status のみの Object は再生しない
- 映像とカタログの datagram には対応しない (LOC の映像は 1 group = 1 unidirectional stream で配送される。同 §4.2)。届いた場合はトラックごとに警告を 1 回出す
- LOC Properties の書式違反は subgroup stream と同じくセッション終了の対象になる (draft-ietf-moq-transport-23 §8.3)

仕様上の扱いは draft-ietf-moq-transport-23 §2.1 (Objects) と §11.2.1 (Object Datagram) を参照すること。

## ログ

環境変数 `RUST_LOG` でログレベルを制御できる。未設定の場合の既定は `info` である。

- `Video player:` 行 (1 秒ごと)
  - `enqueued` / `rendered` / `dropped` / `queue`: 再生機器へ投入した数、描画を観測した数、破棄された数、表示待ちの残り
  - `queue_full` / `late` / `resync`: 破棄の内訳 (キュー上限超過、遅延、復帰時)
  - `drain_recoveries` / `player_resyncs`: moq-sub 側の復帰と raw_player 側の復帰の回数
  - `not_shown`: 提示しなかったフレームの数 (遅れ、同じ表示周期に重なった分を含む)
  - `draw_late`: 目標に合うフレームが無く、遅れて提示した回数
  - `enqueue_to_render=(p50=... p95=... max=...)`: 投入から描画を観測するまでの遅れ
  - `presentation_residual=(p50=... p95=... max=...)`: 記録した表示時刻 − 時間軸が決めた表示時刻
- `Audio jitter buffer:` 行 (5 秒ごと) と `Audio playout drained:` / `Audio playout stopped:` 行: 音声の予算 (`jitter_ms` / `output_lead_ms` / `target_ms`)、不感帯、詰め・補間・伸長の量、穴とアンダーラン、停止時の drain の結果
- 映像の分布 (`enqueue_to_render` / `presentation_residual`) は、表示や同期の判定には使わない。計測値としてだけ出す
