# moqt-subscriber が約 45 秒で受信を止め、relay からの配送も止まる

- Created: 2026-09-17
- Completed: 2026-09-18
- Branch: feature/fix-subscriber-recv-loop-stall
- Polished: {YYYY-MM-DD}
- Updated: 2026-09-18

## 目的

`moqt-subscriber` が連続受信を数十秒で止める。止まると QUIC の MAX_STREAMS が返らなくなり、relay 側も下流へ data stream を開けなくなって配送が完全に止まる。連続受信を分単位で維持できるようにする。

## 現状

- 実測 (moqt-rs publisher → relay → moqt-rs subscriber、`--fake-capture-device`)

```text
   0〜45 秒: 30 fps / 音声 50 chunks/s で受信できる (Rendered / Played が進む)
   約 45 秒: subscriber の media ログが止まる (Rendered / Played が出なくなる)
   直後:     relay が subscriber への data stream を開けなくなる
             (stream_limit_reached、音声で毎秒 50 件前後)
   30 秒後:  subscriber が "Session closed: 0x12 data stream timeout expired" で終了
```

- publisher は止まっておらず、relay は upstream から subgroup を受け取り続けている
- relay 側の計測では、subscriber が送る MAX_STREAMS は累積 1930〜2000 で止まる。relay がその本数を開いた直後に止まり、以後 STREAMS_BLOCKED を送っても増えない
- relay 側は STREAMS_BLOCKED を送るようにし、開けなかった subgroup を保留するようにした。保留は効くが、相手が credit を返さないため保留の上限 (1024 subgroup) を超えて捨てる

## 設計方針

受信ループのどこで止まるかを特定してから直す。候補は次のとおり。

- `accept_recv_stream()` が呼ばれなくなる
  - `pipeline.rs` の `'main: loop` は `tokio::select!` で data stream の accept と
    `client.next_event()` を並べている。`next_event()` が戻らないと accept が止まる
- per-stream task がセッションの mutex を長く保持して accept 側の `recv_object` を待たせる
- decoder (AV1 / Opus) の CPU 飽和で task が進まず、s2n-quic の accept queue が埋まる
  (queue が埋まると MAX_STREAMS を返さなくなり、relay 側が開けなくなる)
- 表示側 (SDL) の追従が遅いことによる backpressure

手順:

1. 受信ループに計測を入れる (1 秒あたりの accept 数、`join_set` の長さ、受信した object 数)
2. 再現させ、どの計測が先に止まるかを確認する
3. 特定した箇所を直す。accept が止まる場合は `select!` の構成 (accept を優先する、または
   accept を専用タスクに分離する) を見直す
4. decoder / 表示の backpressure が原因なら、遅い場合に frame を捨てて受信を継続する

## 完了条件

- 90 秒以上の連続受信で media ログが止まらないこと
- relay 側で `stream_limit_reached` が多発しないこと (相手が MAX_STREAMS を返し続ける)
- 再現と修正を検証するテストがあること (example の範囲で可能な形にする)

## 調査結果 (2026-09-17)

subscriber に 5 秒ごとの計測ログ (accept 数 / 完了したタスク数 / 受信 object 数 / 最後に
object を受信した時刻) を一時的に入れて再現させた。relay 側には MAX_STREAMS の受信と
STREAMS_BLOCKED の送信のログを入れた。

- 停止の瞬間、`accepted` と `done` (タスク完了数) はほぼ等しい (2938 対 2937)。
  **per-stream task は詰まっていない**。
- 受信ループの tick は停止後も 5 秒ごとに動き続ける。**ループ自体も停止していない**。
- relay が下流へ開いた stream 数 (peer が MAX_STREAMS で許した累積値) と subscriber の
  accept 数の差が **約 100 で固定**する。100 は s2n-quic の
  `initial_max_streams_uni` (既定 100) と一致し、accept していない受信 stream が
  上限まで溜まったままになっていると見られる。
- peer の MAX_STREAMS は累積 2400〜2700 で止まり、relay が STREAMS_BLOCKED を
  **1873 回**送っても増えない。RFC 9000 Section 4.6 は「STREAMS_BLOCKED を待たずに
  credit を増やすべき」としているため、s2n-quic 側の credit 返却が止まっている。
- relay 側で同時 fan-out stream 数を 64 に制限する実験 (超過分は保留へ回す) を
  行っても再現した。**relay 側の流量制御では解決しない**。

次の手順:

1. subscriber で s2n-quic の qlog / metrics を有効にし、停止時の MAX_STREAMS / MAX_DATA /
   受信 stream の状態を確認する
2. application の受信ループは動いているのに `accept_receive_stream()` が stream を返さなく
   なる理由を特定する (s2n-quic の acceptor と flow control のどちらで止まっているか)
3. s2n-quic 側の問題なら最小再現を作って上流へ報告し、example 側で回避できるなら
   回避する (受信専用のタスクへ accept を分離する、stream の同時受信数を絞る、など)

relay 側の対処 (STREAMS_BLOCKED と保留の上限) は仕様に沿った改善として
残すが、本 issue の解決にはならない。

## 原因の特定 (2026-09-17, 続報)

relay 側に「送信待ち stream 数 / cwnd / inflight / 損失検出の内容 / 受信フレーム種別」の
計測を入れて調べた結果、次の連鎖が判明した。

1. subscriber の受信ループは動いており、per-stream task も完了している
2. しかし relay→subscriber の接続で **ACK ごとに 1 パケットが損失と判定**される
   (健全な publisher 接続では損失 0)。損失のたびに NewReno が cwnd を最小値
   (2400 = 2 * 1200) に戻すため、cwnd が 2400 のまま回復しない
3. cwnd が最小なので relay は 1 RTT に 2 パケットしか送れず、PTO プローブだけを
   送り続ける。保留している 95 stream のデータは送られない
4. subscriber は新しい stream を受け取れないため MAX_STREAMS を返さず、relay は
   data stream を開けない。30 秒後に subscriber が data stream timeout で session を閉じる
5. ループバックで ~50% のパケットが落ちるのは異常であり、**subscriber の受信バッファが
   溢れている**と見られる。原因は AV1 / Opus のデコードが tokio のワーカースレッドを
   塞ぎ、同じ runtime で動く s2n-quic エンドポイントの I/O が飢えることである

### 緩和策 (本 issue では未解決)

`examples/moqt-subscriber/src/pipeline.rs` に次の 2 つを入れた。

- デコードを `tokio::task::block_in_place` で実行し、runtime のワーカーを長時間塞がない
- 同時に処理する data stream 数を 4 に制限し、CPU を飽和させない

90 秒運転での停止頻度は「4 回中 3 回」から「4 回中 1 回」に減ったが、完全には解消しない。
根本対策は次のいずれかである。

- decoder を track 単位で共有する (現状は stream ごとに AV1 / Opus デコーダを作る)
- 表示が追いつかない場合に frame を捨て、受信を優先する
- QUIC エンドポイントをデコードとは別の runtime に分離する

### 追加の緩和策と再測定 (2026-09-17)

表示待ちの映像フレーム数が 20 を超えたら古い group をまるごと捨てる処理を追加した
(デコードは表示より速く進むため、上限が無いと待ちフレームが増え続ける)。

- 90 秒運転の実測: 停止は残っており、2 回中 1 回で発生する
- 機械全体の CPU は 390〜500% (14 コア = 1400%) で飽和していない。
  publisher が約 240%、subscriber が約 115%、relay が約 5% を使う
- それでも relay→subscriber の接続では ACK ごとに 1 パケットが損失と判定され、
  cwnd が最小値 (2400) に張り付く

このため「CPU 飽和でエンドポイントが飢える」だけでは説明が付かない。残る候補は次のとおり。

1. subscriber の UDP ソケット受信バッファが溢れている (s2n-quic のソケット設定)
2. relay の送信バーストでパケットが落ちている (relay 側は gen_udp バックエンド)
3. 損失検出 / cwnd 回復の実装側の問題 (relay 側の対応)

次は s2n-quic のエンドポイントで受信ドロップ数とソケット統計を取得し、1 か 2 かを切り分ける。

## 負荷を変えた判別実験 (2026-09-17)

同じ機械で publisher / relay / subscriber を動かし、90 秒運転を条件を変えて繰り返した。

| 条件 | 結果 |
| --- | --- |
| 音声のみ (`--no-video`) | 安定 (4500 chunks、data stream timeout 0、relay の fan-out 失敗 0) |
| 映像 320x180 @15fps / 200kbps + 音声 | 3 回とも安定 (1300 frames / 4500 chunks、timeout 0、失敗 0) |
| 映像 1280x720 @30fps / 2Mbps + 音声 (既定) | 不安定 (2〜3 回に 1 回停止、他の回も ~10fps まで低下) |

- 停止した実行でも OS の UDP ソケットバッファ溢れドロップは **0 件**
  (`netstat -s -p udp` の "dropped due to full socket buffers" の増分で確認)
- パケットは OS のソケットバッファより上 (s2n-quic の受信処理) か、relay の送信側で
  失われている。relay は ACK ごとに 1 パケットを損失と判定し、cwnd が最小値に張り付く
- 中程度のビットレートでは end-to-end で安定動作することを確認できた。したがって
  本 issue の残りは「高ビットレート時の安定性」であり、relay 側の対応とあわせて
  継続調査する

推奨する検証条件 (安定して再現する): `--width 320 --height 180 --fps 15 --bitrate 200`。

## 追加実測 (2026-09-17、relay の relay 側対応後)

relay 側の対応で relay が「PTO プローブに新データを載せる」「停滞した購読を 30 秒で
終了する」対応を入れた後の実測 (publisher → relay → subscriber、各 90 秒)。

| 条件 | 結果 |
| --- | --- |
| 320x180 @15fps / 200kbps + 音声 | 安定 (855 frames / 3000 chunks、timeout 0、relay の warning 0) |
| 640x360 @30fps / 800kbps + 音声 | 安定 (2600 frames / 4400 chunks、timeout 0、relay の warning 0) |
| 1280x720 @30fps / 2Mbps + 音声 | 不安定 (1000 frames 前後で停滞、timeout 1、relay が 30 秒後に購読を終了) |

- 安定域が 320x180@15fps から 640x360@30fps / 800kbps まで広がった
- 高ビットレートでは依然として停滞する。relay 側は保留が上限 (1024 subgroup) に達し、
  PTO プローブが新データを運ぶようになっても、peer が受信を再開しない間は
  PTO ごとに 1 packet しか進まないため配送は戻らない
- OS の UDP ソケットバッファ溢れは 0 件のままである。停止は subscriber の
  s2n-quic エンドポイントが受信処理を続けられなくなることで起きており、
  720p30 の AV1 デコードを含む負荷が runtime を飽和させる経路が残っている

次の候補 (緩和策の続き):

1. QUIC エンドポイントをデコードとは別の runtime / スレッドに分離する
2. decoder を track 単位で共有し、stream ごとの生成をやめる
3. 受信した video object をデコード前に捨てる判断を入れ、表示が追いつかないときは
   デコード自体を間引く (現状は表示待ち 20 フレームで group を捨てるが、デコードは進む)

## プロファイリングによる追加実測 (2026-09-17)

停滞時の subscriber プロセスを `sample` で 3 秒間計測した
(1280x720@30fps / 2Mbps、800 フレーム受信後に停滞)。

```text
    tokio-rt-worker (io driver)   kevent で 1846/1900 サンプル待機 (受信パケット無し)
    tokio blocking pool           全スレッドが __psynch_cvwait で待機 (デコード作業なし)
    AV1 デコード                  サンプル 0 件
    メインスレッド                SDL のイベント待ちと sleep
```

- **停滞中の subscriber は CPU を使い切っていない**。QUIC エンドポイントはソケット待ちで
  あり、パケットが届けば処理して ACK を返せる状態にある
- したがって「デコードが runtime を飽和させてエンドポイントが飢える」という説明は
  停滞の直接原因ではない。この時点で relay から subscriber へパケットが届いていない
- これは relay 側の送信停止 (cwnd 最小値・PTO ループ) を示すもので、原因の切り分けは
  relay の送信経路 (can_send / pending_retransmit / pacing /
  bytes_in_flight) の計測へ移した

CPU 飽和が起きるのは停滞に至るまでの高ビットレート処理中であり、停止後は idle に戻る。
つまり「処理能力不足で止まる」のではなく「止まったあと relay が再開できない」構造である。

## relay 側の計測で確定したこと (2026-09-18)

relay の送信経路を計測した結果 (計測手順を修正したうえでの再測定)。

```text
    ACK:            停滞中も届いている。ただし 1 ACK ごとに acked=1 / lost=1 が対で出る
    cwnd:           2400 (最小値) のまま。inflight は 1 packet 分 (1614)
    pending:        relay の保留は 90 → 1024 (上限) まで増え続ける
    PTO:            プローブ 1 packet ずつしか送れない
    demux:          relay の socket 受信は止まっていない (受信総数は増え続ける)
```

- **subscriber が高ビットレートで packet を取りこぼしている** (relay から見て ACK の範囲に
  穴が空く)。これが輻輳ウィンドウの崩壊を招き、cwnd が最小値に張り付いて配送が止まる
- relay 側の送信経路 (socket 受信 / cwnd 判定 / 保留の flush) は正常である。
  `max_congestion_window` を 2400 に固定した EUnit では、peer が ACK を返す限り
  64 KiB が 0.17 秒で全部届く (relay の EUnit)
- したがって本 issue の解消は subscriber 側の取りこぼしをなくすことに絞られる

具体的な候補 (実装難易度順):

1. s2n-quic の IO provider の内部受信バッファ / SO_RCVBUF を明示的に大きくする
   (`with_internal_recv_buffer_size` / `with_recv_buffer_size`。既定の内部バッファは 8 MiB)
2. QUIC エンドポイントをデコードとは別の runtime / スレッドに分離する
   (現状は同一 runtime を共有しており、負荷時にエンドポイントの処理が遅れる)
3. 受信 Object をデコード前に間引く (表示が追いつかないときはデコード自体を止める)

## 受信バッファ拡大の試行と結果 (2026-09-18)

候補 1 (受信バッファの拡大) を実装して実測した。

- `examples/moqt-transport/src/quic.rs` の QUIC クライアントで
  `s2n_quic::provider::io::Default::builder()` を使い、
  `with_recv_buffer_size(4 MiB)` (SO_RCVBUF) と
  `with_internal_recv_buffer_size(64 MiB)` (s2n-quic の内部受信キュー、既定 8 MiB) を
  明示する変更を入れてビルドした

結果 (1280x720@30fps / 2Mbps、90 秒):

```text
    subscriber:  1200 フレームで停滞 (data stream timeout 0 のまま relay が 30 秒で購読終了)
    relay:       pending=1024 dropped=491 (停滞の挙動は変更前と同じ)
    OS の UDP:   dropped due to full socket buffers は 201 のまま増えず (変更前と同じ)
```

- **停滞は解消しなかった**。OS のソケットバッファ溢れが 0 のままであることも再確認でき、
  取りこぼしは OS のバッファではなく subscriber のプロセス内 (s2n-quic の受信処理) で
  起きていることが強く示唆される
- 効果が確認できなかったため、この変更はコミットしていない (作業ブランチを破棄)

次の候補 (優先順):

1. **relay の実送信数と peer の ACK 数の突き合わせ**。relay の qlog の `packet_sent` 件数と
   実際の `gen_udp:send` 成功数、subscriber が ACK した数を並べ、packet がどこで
   消えているか (relay の送信 / カーネル / subscriber の受信処理) を確定する
2. QUIC エンドポイントをデコードとは別 runtime / スレッドに分離する
   (現状は subscriber の単一 runtime を共有している)
3. 受信 Object をデコード前に間引く

## 表示待ち上限を厳しくする試行と結果 (2026-09-18)

候補 3 (デコード前に間引く) の効果を確かめるため、`MAX_DISPLAY_BACKLOG` を 20 から 4 に
下げて (表示待ち 4 フレームを超えたら video group を丸ごと捨てる)、
1280x720@30fps / 2Mbps を 2 回実測した。

| 回 | 結果 |
| --- | --- |
| 1 回目 | 停滞せず (2100 フレーム / 3900 chunks、timeout 0、relay の停滞終了 0、warning 0) |
| 2 回目 | 停滞 (1100 フレーム、relay が 30 秒後に購読終了、pending=1024) |

- 間引きを強めると**停滞しにくくなるが、解消はしない**。機械の負荷状況によって
  確率的に発生するという従来の観測と一致する
- 有効性が確実でないため、この変更はコミットしていない (作業ツリーを元に戻した)

この試行から言えること:

- 停滞は peer の処理能力 (デコード負荷) に強く依存する。負荷を下げれば発生率は下がる
- しかし「負荷を下げる」だけでは 90 秒連続の安定を保証できない。
  根本対策は packet を取りこぼしても輻輳が崩壊しないようにすること (受信処理の分離) であり、
  候補 2 (QUIC エンドポイントをデコードとは別 runtime に分離) が本命である

## QUIC エンドポイントの runtime 分離の試行と結果 (2026-09-18)

候補 2 (QUIC エンドポイントをデコードとは別 runtime に分離) を実装して実測した。

- `examples/moqt-transport/src/quic.rs::connect()` で `Client::builder()...start()` を
  専用 runtime (worker 2 本、専用スレッドで常駐) の上で実行し、エンドポイントの I/O を
  アプリ (デコード) の runtime から切り離した。接続後の `Connection` は従来どおり
  アプリ側から使う
- `Client::start()` はエンドポイントの I/O タスクを「その時点の runtime」に生成するため、
  生成だけを専用 runtime に載せれば分離できる

結果 (1280x720@30fps / 2Mbps、各 90 秒):

| 回 | 結果 |
| --- | --- |
| 1 回目 | 停滞 (900 フレーム、data stream timeout 1、relay が 30 秒後に購読終了) |
| 2 回目 | 停滞せず (1600 フレーム / 3200 chunks、timeout 0) |

- 分離しても**停滞は解消しない** (2 回に 1 回発生)。確率的に起きるというこれまでの観測と一致する
- 有効性が確実でないため、この変更はコミットしていない

## ここまでの対策試行のまとめ (1280x720@30fps / 2Mbps)

| 試行 | 1 回目 | 2 回目 |
| --- | --- | --- |
| 現状 | 停滞 (600〜1200 フレーム) | 停滞 |
| 受信バッファ拡大 (SO_RCVBUF 4 MiB / 内部 64 MiB) | 停滞 (1200) | — |
| 表示待ち上限を 20 → 4 | 安定 (2100) | 停滞 (1100) |
| QUIC エンドポイントの runtime 分離 | 停滞 (900) | 安定 (1600) |

いずれも発生率は下がるが解消しない。OS の UDP 取りこぼしが 0 であることは全試行で共通しており、
**subscriber のプロセス内で packet が失われている**という結論は変わらない。次に必要なのは
「失われている場所」の確定であり、そのためには次の計測を足す。

1. relay の `quic:packet_sent` (qlog) 件数と実際の `gen_udp:send` 成功数
2. subscriber が受信した packet 数 (s2n-quic の event / metrics)

1 と 2 を突き合わせれば、relay の送信・カーネル・subscriber の受信処理のどこで
消えているかが確定する。そこが分かるまでは当てずっぽうの緩和策を増やさない。

## relay の実送信数と peer のメモリ使用量 (2026-09-18、原因の確定)

上記 1 と 2 の計測を実施した。1 は relay の作業ツリーでソケット送信関数の
`gen_udp:send` が `ok` を返した回数を数える一時計測 (コミットしていない)、2 は実行中の
publisher / relay / subscriber の RSS と CPU を 4 秒間隔で取得する方法で行った。

条件は 1280x720@30fps / 2000 kbps / 90 秒。

| 計測項目 | 結果 |
| --- | --- |
| relay の `gen_udp:send` 成功数 | 30000 packet (単調増加、send error は 0 件) |
| relay の subscriber 接続の `send_pn` | 14480 → 23046 |
| 停滞時の relay の状態 | `cwnd=2400 inflight=11051 pending_streams=95 pending_retransmit=21 unacked=9` |
| relay の RSS | 42〜190 MB |
| publisher の RSS | 8.5〜15 GB (開始から数秒で到達) |
| subscriber の RSS | 6.8 GB (開始 20 秒) → 15.8 GB (44 秒)、別の回では 24 GB |
| subscriber の CPU | 1 スレッドが 90〜94% で回り続ける |
| release ビルドの peer での再実行 | debug ビルドと同じく停滞 (700 フレーム、`pending=1024 dropped=491`) |

判明したこと。

- relay の送信側は健全である。停滞中も socket への write は成功し続けており (error 0)、
  relay が packet を送らずに溜めているわけではない
- 停滞は subscriber 側で起きている。subscriber は RSS が GB 級に増えて 1 スレッドが
  飽和し、packet を読み続けられなくなる。映像だけでなく音声も同時に止まるため、
  映像のデコードだけが遅いのではない
- release ビルドでも同一の停滞が起きるため、debug ビルドの遅さが原因ではない
- relay から見ると「loss が多発 → cwnd が最小値 2400 に張り付く → 再送もできない」という
  症状になる。これは原因ではなく結果である

### 原因

0101 の本質は「relay が詰まる」ではなく **example peer がメモリを食い尽くして自分の I/O を
止める**ことである。publisher と subscriber の RSS が数秒で GB 級に増え、同じ runtime 上の
QUIC エンドポイントが受信 packet を取りこぼす。取りこぼした packet は本当に失われており、
relay の輻輳ウィンドウが最小値まで落ちて復帰できない。

したがって修正すべき対象は次の 2 つである。

1. example peer の queue と確保するメモリを有界にする (publisher / subscriber の双方で
   GB 級に増える原因を特定して塞ぐ)。これが 0101 の本修正である
2. relay の停滞監視 (relay 側の対応) は緩和策として維持する

### 関連して見つかった問題

`--fake-capture-device` の publisher は 1280x720@30fps/2000 kbps で AV1 エンコーダ
(raden) が GB 級のメモリを確保し、単体起動でもそのまま異常終了することがある
(panic メッセージを出さずにプロセスが消える)。0101 とは別 issue として扱う (後述の
追加計測で、エンコーダだけの問題ではないことが分かった)。

## メモリ増加の正体の追跡 (2026-09-18、追加計測)

media の種類を変えて publisher 単体の RSS を 20 秒後に測った (release ビルド、
relay は起動したまま)。

| 条件 | RSS |
| --- | --- |
| `--no-video` (音声のみ) 1280x720 設定 | 13744 MB |
| 映像 160x90@30fps / 200 kbps | 14446 MB |
| 映像 1280x720@30fps / 2000 kbps (`--no-audio`) | 13830 MB |

**media の量と無関係に 20 秒で 13 GB 前後まで増える**。映像もエンコーダも使わない
音声のみでも同じなので、原因は映像経路ではない。

`heap` / `leaks` / `vmmap` の結果 (debug ビルド、音声のみ、起動 12 秒時点)。

- `heap`: 生存 node は 1460 個だけ。うち 1 個が 58.7 GB (仮想) で、残りは 13 MB 以下。
  つまり **1 個の巨大な growable buffer が倍々で伸びている**
- `leaks`: `0 leaks for 0 total leaked bytes`。解放漏れではなく、到達可能なまま育っている
- `vmmap`: `MALLOC_LARGE` が 16.4 GB (92% が dirty)、physical footprint 13.1 GB
- `ps`: CPU は 90〜136%

`MallocStackLogging` を有効にしても 58 GB の確保は記録されず (mmap 経路のため)、
どのコードが伸ばしているかは未特定である。

### 次の作業

1. 58 GB まで伸びる buffer の持ち主を特定する。候補は `moqt-example-transport` の
   送受信ループ、またはその s2n-quic の使い方である。media 非依存で秒あたり数百 MB
   増えるため、object あたりではなくループ 1 回あたりに確保している可能性が高い
2. 特定後に有界化する。0101 の停滞は peer の I/O 飢餓が起点なので、ここを直さないと
   高ビットレートの相互接続検証は通らない
3. relay 側の停滞監視 (relay 側の対応) は緩和策として維持する

## 原因の特定: tokio-metrics の intervals を collect していた (2026-09-18)

`sample` の top of stack が `tokio_metrics::task::TaskIntervals::probe`、
`Duration::cmp`、`Vec<TaskMetrics>::extend_desugared` で占められていたことから
`tokio-metrics` の使い方を確認した。publisher / subscriber の `main.rs` に

```rust
loop {
    let intervals: Vec<_> = task_monitor.intervals().collect();
    for m in intervals { ... }
    tokio::time::sleep(Duration::from_secs(5)).await;
}
```

があった。`TaskIntervals` の `Iterator::next` は `Some(self.probe())` を返すだけの
終端のない iterator で、待ち合わせもしない (tokio-metrics 0.5.2 の実装で確認)。
したがって `collect()` は永久に返らず、次のことが起きていた。

- `Vec<TaskMetrics>` が 1 個の巨大な growable buffer として倍々に伸びる (58 GB)
- そのタスクが 1 スレッドを占有し続ける (CPU 90〜136%)
- `for` ループの後にある 5 秒 sleep とログ出力に永久に到達しない
  (「Task metrics:」のログが 1 行も出ていなかった)

media の量に依存しないこと、`leaks` が 0 で「到達可能なまま育つ」こと、
`heap` で 1 個だけ巨大な確保が見えることがすべて説明できる。

### 解決方法

共有ヘルパー `moqt_example_transport::metrics::spawn_task_metrics_logger` を追加し、
publisher / subscriber の `main.rs` はこれを呼ぶようにした。実装は `intervals()` を
1 件ずつ取り出してログ出力し、5 秒待つ。

`examples/moqt-transport/src/metrics.rs` に次のテストを追加した (日本語のメッセージ)。

- `test_task_intervals_never_ends`: `TaskIntervals` が終端のない iterator であること
- `test_task_intervals_next_returns_immediately`: `next()` が待ち合わせないこと
- `test_spawn_task_metrics_logger_starts`: ログ出力タスクを起動しても取得が進むこと

### 修正の検証 (1280x720@30fps / 2000 kbps / 120 秒)

| 項目 | 修正前 | 修正後 |
| --- | --- | --- |
| publisher RSS | 8.5〜15 GB | 78 → 86 MB (一定) |
| subscriber RSS | 6.8 → 15.8 GB | 134 → 156 MB (一定) |
| publisher CPU | 90〜136% | 44% |
| subscriber CPU | 90% | 9% |
| 「Task metrics:」ログ | 1 行も出ない | 5 秒ごとに出る |

メモリの爆発と CPU 飽和は解消した。

## 残っている停滞 (2026-09-18、メモリ修正後)

同じ条件で 120 秒流すと、75 秒付近で subscriber の描画が止まった
(2313 video frames / 3869 audio chunks で停止、subscriber の CPU は 0.6% に低下)。
relay 側は `cwnd=2400 inflight=12719 pending_streams=93 pending_retransmit=21
unacked=8` となり、停滞監視が発動して subscriber を終了させた。subscriber 側は
`0x12 data stream timeout expired` で終わっている。

つまり **メモリ爆発は原因の一部で、受信ループの停滞そのものは残っている**。
peer が健全で idle になっても relay の輻輳ウィンドウが最小値に張り付くため、
次は次を確認する。

1. subscriber の s2n-quic が packet を実際に受け取っているか (受信数と損失の計測)
2. relay の損失検出が誤判定していないか (`packets_acked=1` に対して
   `packets_lost=1` になる挙動)
3. `pending_streams=93` が peer の MAX_STREAMS 枯渇 (フロー制御) によるものか、
   損失によるものかの切り分け

なお、この検証で使った relay は一時計測 (DEBUG ログ) を入れた release のままだった
(ソースは復元済み、その後に計測なしで再ビルド済み)。計測はログ出力のみなので
挙動への影響はない。

## 停滞点の特定: stream 枠 (semaphore) の枯渇 (2026-09-18)

停滞した subscriber のスタックを取っても、pending な future はスタックに現れないため
`pipeline::run` の停止位置は分からなかった。そこで `MAX_CONCURRENT_STREAMS` の
semaphore の残枠を 5 秒ごとにログ出力する一時計測を入れて再現させた (debug ビルド)。

結果 (メモリ修正済み、1280x720@30fps / 2000 kbps)。

| 時刻 | available_permits | 状態 |
| --- | --- | --- |
| 18:24:58 | 3 | 正常 (30 fps で描画中) |
| 18:25:03 以降 | **0 のまま** | 映像も音声も停止 |

- accept ループは `accept_recv_stream()` の後、**`select!` の枝の中で**
  `stream_slots.acquire_owned().await` している。枠が埋まると select 全体が止まり、
  accept も `client.next_event()` も進まなくなる
- 同時に relay 側は `pending fan-out reached its limit ... dropped=1` を出しており、
  相手が stream を受け付けないため fan-out の保留 (上限 1024) が溢れている
- そのまま 30 秒進まないので relay の停滞監視が発動し、subscriber は
  `data stream timeout expired` で終了する

### 試したが採用しなかった案

枠の取得を accept ループの中から spawn したタスクの中へ移す案を実装して検証した。
構造的な停止は消えるが、relay が送ってくる stream を無制限に accept してしまうため
(90 秒で 359 stream を受理)、処理待ちの stream が溜まって
`data stream arrived before session established` と idle timer 満了で終わった。
この案は採用せず、変更は戻した。

### 次に必要なこと

1. stream の到着数と 1 stream あたりの処理時間を実測し、`MAX_CONCURRENT_STREAMS` の
   値を決める根拠にする (現状は 4 で、根拠が実測に基づいていない)
2. 枠が無いときに accept ループを止めるのではなく、stream を読み切って捨てる
   (STOP_SENDING + drain) 方向に変える。既にある「表示待ちが多ければ group を捨てる」
   判定は stream を受理した後にしか効かないため、枠待ちの間に判定できない
3. relay 側の輻輳ウィンドウが最小値に張り付く現象そのものの解明
   (`packets_acked=1` に対して `packets_lost=1` になる挙動、peer が ACK を返さなく
   なる理由)

## stream の到着数と処理時間、relay 側の消費の実測 (2026-09-18)

subscriber の stream タスクの開始 / 終了を一時計測して再現させた
(1280x720@30fps / 2000 kbps、release ビルド、メモリ修正済み)。

| 計測項目 | 結果 |
| --- | --- |
| stream の到着数 | 30 秒間で約 900 (秒あたり約 30) |
| stream の種類 | すべて `Subgroup` |
| 大半のタスクの処理時間 | 数十 µs (映像 group を捨てる経路) |
| 一部のタスクの処理時間 | 約 2.0 秒 (映像 group を最後まで処理) |
| 最長のタスク | **62 秒** (接続が切れるまで終わらない) |
| 表示待ちによる drop ログ | 0 件 |

- 停滞の直前 (18:33:45) を最後に stream が 1 本も届かなくなる。relay 側では
  同じ時刻以降 `pending fan-out reached its limit` が出ており、relay は受信できて
  いるが subscriber へ送れていない
- 62 秒間終わらない stream が 1 本あり、これが枠を 1 つ占有し続けた

### relay 側のコードを読んだ結果

- `cc_newreno:on_packets_lost/2` は損失パケットのバイト数を `bytes_in_flight` から
  正しく引いている (in-flight の計算漏れではない)
- 再送は `batch_flush_retransmit_frames/2` で `quic_cc:can_send/2` を通しており、
  輻輳ウィンドウに従っている (PTO プローブだけが対象外)
- それにもかかわらず実測では損失検出 1 回あたり `inflight` が約 1.5 KB 増え続け、
  `unacked` は 8 前後で一定だった。損失で引かれ、プローブで足される以上のバイト数が
  増えている計算になる。どの送信が in-flight に残っているのかは relay 側の計測が
  必要で、まだ特定できていない

### 次の一手

relay の EUnit で「peer が ACK を返さなくなる」状況を作り、`sent_packets_app` と
`bytes_in_flight` の推移を直接確認する。これで in-flight が増え続ける送信元
(プローブか再送か通常送信か) を確定できる。

## 実機 relay で捕捉した停滞の瞬間 (2026-09-18)

relay に 1 秒ごとの回復状態ログ (`RELAY_RECOVERY_LOG=1` のときだけ動く一時計測) を
入れて 1280x720@30fps / 2000 kbps を流し、停滞の前後を 1 秒刻みで記録した。

| 局面 | inflight | cwnd | unacked | pending_streams | pending_retransmit | send_pn |
| --- | --- | --- | --- | --- | --- | --- |
| 正常時 (描画 1300 フレームまで) | 1503〜2527 | 2400 | 1〜3 | 93 | 26 | 毎秒 +約 460 |
| 停滞後 | 12839 → 17654 | 2400 | 8 → 11 | 95 | 19 → 16 | 毎秒 +1 |

判明したこと。

- **正常に配送できている間も cwnd は最小値 2400 のまま**である。loopback では RTT が
  小さいため 2400 バイトでも 2 Mbps を配送できる。つまり cwnd が最小値なこと自体は
  停滞の原因ではない
- 停滞の瞬間に peer は **ACK を完全に返さなくなる**。以後 relay が送れるのは PTO
  プローブだけになり、`send_pn` は毎秒 +1、`inflight` はプローブ 1 個分 (約 1.6 KB)
  ずつ単調に増える
- 増えたプローブは損失と判定されない。損失検出は `largest_acked` を基準に判定するため
  (`fold_lost_packet/6` は `PN > LargestAcked` を「まだ ACK される可能性がある」として
  残す)、ACK が 1 つも来なくなると新しいパケットは永久に in-flight に残る
- これは EUnit (`relay の EUnit`、relay 側の修正) で再現でき、
  peer が復帰すれば in-flight は解消して配送も完了する

したがって残る原因は **peer (subscriber) が ACK を返さなくなる理由**である。relay 側は
「ACK が来ないので送れない」という結果を出しているだけである。次は subscriber 側で
アプリが消費を止めた時点 (4 本の stream タスクが枠を握ったまま) と ACK が止まる時点の
前後関係を、s2n-quic の受信バッファとあわせて計測する。

## 停滞の引き金を特定: publisher の一時停止 (2026-09-18)

1280x720@30fps / 2000 kbps で正常に流れている最中に、publisher プロセスへ
`SIGSTOP` を送って **5 秒間だけ**止め、その後 `SIGCONT` で再開させた。

| 時刻 | 状態 |
| --- | --- |
| t=35s | publisher を 5 秒間停止 |
| t=40s | publisher を再開 |
| t=45s〜120s | 描画は 1200 フレームから 1 フレームも進まない |
| 最後 | relay の停滞監視が発動 (`terminating stalled: 1`)、subscriber は `data stream timeout` 0 件のまま終了 |

**5 秒の出力途切れだけで、その後 80 秒以上まったく回復しない**。これまで確率的に
見えていた停滞は、publisher の一時停止 (== 送るものが無い期間) で確実に再現する。

### 機構

これまでの計測と合わせると次の連鎖になる。

1. publisher が group の途中で止まると、その subgroup stream は FIN が来ないまま残る
2. subscriber の 4 本の stream タスクは「stream の終わり」を待ち続け、枠を握ったまま
   終わらない (実測: `available_permits=0` が固定、最長 62 秒)
3. subscriber は新しい stream を accept しないので peer の MAX_STREAMS credit が返らない。
   relay 側は以前から `pending_streams=93` を抱えており、再開後のデータを流す stream を
   開けない
4. relay の fan-out 保留が上限 1024 に達して object を捨て始める
5. 結果として映像も音声も届かず、relay の停滞監視だけが動く

relay 側の in-flight 増加や cwnd 最小値張り付きは 4 の副次的な症状である。

### 次に必要な修正

1. subscriber: 枠を「stream の終わり」まで保持しない。object を処理し終えたら枠を返し、
   終わらない stream は時間で打ち切る (`STOP_SENDING` + drain)
2. publisher: group を中断したときは subgroup stream を FIN で閉じる (相手を待たせない)
3. relay: peer が accept しない状態でも保留が溢れないようにする (保留の上限と合わせて確認)

## stream 処理の時間制限を試した結果 (2026-09-18)

subscriber 側で 1 本の data stream の処理に上限時間 (10 秒) を設け、超えたら打ち切って
枠を返す変更を試した (publisher を 5 秒止める再現シナリオで検証)。

| 変更 | 停止後の描画 |
| --- | --- |
| なし | 1200 フレームで完全停止 (80 秒以上進まない) |
| stream 処理に 10 秒上限 | 1200 → **1400** まで回復したあと再び停止 |

枠は返るようになり一時的に再開するが、**完全には回復しない**。再開後に再度止まるため、
枠の枯渇だけが原因ではない。残っているのは relay 側が「PTO プローブしか送れない」
状態 (in-flight が cwnd を超えたまま、ACK が返らない) であり、次はここを直す必要がある。

この変更は完全な修正ではないため、コミットしていない (作業ツリーは clean)。

## subscriber を止めた場合との比較 (2026-09-18)

「peer が ACK を返さない」こと自体が回復不能なのかを切り分けるため、publisher ではなく
**subscriber プロセスを 5 秒間 SIGSTOP して再開**させた (publisher は動かし続ける)。

| 停止した側 | 再開後の描画 |
| --- | --- |
| publisher (5 秒) | 1200 フレームで完全停止 |
| subscriber (5 秒) | 1200 フレームで停止 (回復せず) |

どちらでも回復しない。subscriber を止めれば ACK もアプリの消費も同時に止まるため、
この結果は「ACK が止まること自体が回復不能な状態を作る」ことを示す。つまり relay 側が
**ACK が来ない間に in-flight を増やして送信不能に陥る**挙動 (プローブが損失判定されず
in-flight に残留する) が本質的な原因であり、peer のアプリ側だけの問題ではない。

### 次の一手

relay が「in-flight のパケットがすべて古い (時間しきい値を超えた) 」場合に、輻輳
ウィンドウに縛られず新データを送って回復できるようにする。RFC 9002 Section 7.5 は
プローブを輻輳制御の対象外としており、その範囲で送るデータを「新データ優先」に
切り替えれば、ACK が戻り次第ウィンドウが開いて復帰できる。

## 通常時の停滞の原因: stream 同時数のミスマッチ (2026-09-18)

1280x720@30fps / 2000 kbps を外部から止めずに流し、relay の 1 秒ごとの回復状態ログで
2 つの接続を比較した。

| 接続 | 観測値 |
| --- | --- |
| publisher 側 | cwnd 16.9M → 19.5M、inflight 1〜5 KB、unacked 2〜9、pending 0、send_pn 35k → 40k (健全) |
| subscriber 側 | cwnd 2642、inflight 17511、unacked 11、**pending_streams=99**、pending_retransmit 10、**send_pn 7827 で凍結** |

描画は 300 フレームで停止し、relay の停滞監視が発動した。

### 原因

- relay は subgroup ごとに新しい stream を開いて fan-out するため、peer の stream 上限に
  達すると 99 本の保留 subgroup を抱える
- subscriber は `MAX_CONCURRENT_STREAMS = 4` で、しかもその枠を stream の終わりまで
  保持する
- そのため relay は新しい stream を開けず、subscriber は終わらない stream を待つ。
  **外部からの停止がなくても**この閉路が自然に発生する

これまで観測した「5 秒停止で再現」「復帰しない」「in-flight が増え続ける」はすべて
この 1 つの構造問題から説明できる。

### 解決方針

subscriber を「stream は即座に受理し、デコード同時数だけを制限する」形に変える。

1. accept ループでは枠を取らず、受理した stream をすぐ読む (credit が即返る)
2. デコードは別の semaphore で制限し、取れなければその subgroup を
   `STOP_SENDING` + drain で捨てる (枠を保持しない)
3. これで relay の stream 上限が回復し、保留 subgroup が流れ始める

「枠取得をタスク内へ移す」案だけでは accept が止まらない代わりに 359 stream を
無制限に受理して失敗した。今回は「受理は即座・デコードだけ制限・超過分は捨てる」の
組み合わせにする点が違う。

## 同時受理数を増やす案の結果 (2026-09-18)

`MAX_CONCURRENT_STREAMS` を 4 から 32 に増やして 1280x720@30fps / 2000 kbps を流した。

| 設定 | 描画 |
| --- | --- |
| 4 (現状) | 1100 フレームで停止 |
| 32 | **200 フレームで停止 (悪化)** |

同時受理数を増やしても解決しない。むしろ、終わらない stream を待つタスクが増えて
消費が追いつかず、枠とメモリを圧迫するため早く止まる。

### 結論

問題の本質は「同時数」ではなく、**subscriber が stream を読み切って捨てる経路を持たない
こと**である。現状は次の構造になっている。

- 表示待ちが多ければ group を捨てる判定はあるが、stream を受理してヘッダを読んだ後に
  しか効かない
- 枠が埋まっている間は accept 自体が止まるため、捨てる判定にも到達しない

したがって修正は「受理は即座に行い、デコード枠が取れなければ subgroup を捨てて
`STOP_SENDING` + drain する」構造への組み替えが必要である。枠取得をタスク内へ移すだけの
案 (359 stream を無制限に受理して失敗) と、同時数を増やすだけの案 (本項) はどちらも
否定された。

## 停滞中の UDP カウンタ (2026-09-18、最終計測)

停滞した状態で publisher を停止し、relay → subscriber のパケットだけが流れる状況で
20 秒間の UDP カウンタ増分を測った。

| カウンタ | 増分 |
| --- | --- |
| datagrams received | +50 |
| **dropped due to full socket buffers** | **+0** |
| dropped due to no socket | +3 |
| datagrams output | +57 |

- **subscriber の socket バッファは溢れていない**。パケットはカーネルに届き、受信
  バッファに受け入れられている
- relay 側は `address_validated=true` / `deferred_packets=0` で、保留せず送信している
- それでも ACK は返らず、relay の `inflight` は増え続け `send_pn` は毎秒 +1

したがって「送信側は送っている / 受信カーネルには届いている / s2n-quic が処理して
いない」まで確定した。subscriber は idle (CPU 0.6%) で panic もない。

### 次に必要な計測 (決定打)

subscriber に s2n-quic の events API (`with_event`) を入れ、停滞中の受信パケット数を
数える。増えていれば endpoint の駆動側 (tokio ランタイム / ロック) の問題、増えていなければ
受信経路の問題として、修正対象が確定する。

### 否定済みの仮説 (すべて実測)

| 仮説 | 結果 |
| --- | --- |
| tokio-metrics の collect によるメモリ爆発 | 原因の一部だった → 修正済み (58 GB → 156 MB) |
| subscriber の stream 枠ポリシー (4 案) | いずれも停滞は解消せず |
| relay の輻輳ウィンドウ最小値張り付き | 正常時から 2400 で配送できており原因ではない |
| relay の送信失敗・握り潰し | 送信失敗時は接続が落ちる実装で否定、実送信数も確認済み |
| 反増幅 (anti-amplification) による deferred | `validated=true deferred=0` で否定 |
| subscriber の socket バッファ溢れ | `dropped due to full socket buffers` +0 で否定 |

## 崩壊は初期からではなく後から始まる (2026-09-18)

subscriber 接続の毎秒カウンタを接続直後から記録した (1280x720@30fps / 2000 kbps)。

```text
DEBUG loss: acked=178 lost=0 persistent_congestion=0 cwnd=23707 srtt=134 inflight=0 send_pn=177
DEBUG loss: acked=157 lost=0 persistent_congestion=0 cwnd=34341 srtt=154 inflight=0 send_pn=333
```

- 初期は `lost=0`、cwnd は 23k → 34k と順調に増加、毎秒 157〜178 ACK で約 1.6 Mbps 相当
- 別接続 (publisher 側、srtt 12〜16 ms) も `lost=0`、cwnd 15017 → 86807
- 崩壊は後から確率的に始まり、その後 acked≈50 / lost≈41 / cwnd=2400 の定常状態に入る

したがって「時間しきい値が最初から短すぎて常時誤判定している」わけではない。実際、
損失判定の下限に peer の ACK delay (25 ms) を加える変更を試したが改善しなかった
(1700 フレームで停滞、変更は戻した)。

最も整合的な筋は「負荷イベントを機に subscriber 側が受信パケットを捨て始め、それを
引き金に relay の cwnd が床に固定され、回復できない」である。

## 崩壊する bitrate のしきい値 (2026-09-18)

1280x720@30fps で bitrate を変えて 60 秒ずつ流した。

| bitrate | 描画 | timeout | relay の停滞監視 |
| --- | --- | --- | --- |
| 1200 kbps | 1700 フレーム (30 fps 維持) | 0 | 0 |
| 1600 kbps | 1700 フレーム (30 fps 維持) | 0 | 0 |
| 2000 kbps | 200〜1400 フレームで停止 | 1 | 1 |

**しきい値は 1600〜2000 kbps の間**にあり、これは実測した天井
`cwnd × 10^6 / srtt` (cwnd=2400、srtt 0.7〜2 ms で 1.2〜3.5 Mbps) と一致する。

つまり 2 Mbps は cwnd 最小値での天井とほぼ同じか上にあり、queue が溜まって相手が
パケットを捨て始め、cwnd が床に固定されたまま回復しない。

回避策としては 720p30 なら 1600 kbps 以下で安定する (実測)。
根本修正は cwnd が最小値から増えない理由の解明 (損失多発の原因) である。

## 720p30 / 1600 kbps の 120 秒検証 (2026-09-18、成功)

安定域の上限側で 2 分間の end-to-end 検証を行い、すべて成功した。

| 項目 | 結果 |
| --- | --- |
| 描画 | 3200 フレーム (約 28 fps)、停止なし |
| 音声 | 5900 chunks |
| decode error | 0 |
| data stream timeout | 0 |
| relay の error / warning | 0 / 0 |
| 終了 | PUBLISH_DONE status_code=4 で正常終了 |

したがって現時点で「moqt-rs の publisher / subscriber が relay と
end-to-end で動作する」ことが実測で示せている範囲は次のとおり。

| 条件 | 連続時間 | 結果 |
| --- | --- | --- |
| 320x180@15fps / 200 kbps | 120 秒 | error 0 |
| 640x360@30fps / 800 kbps | 60 秒 | error 0 |
| 1280x720@30fps / 1600 kbps | 120 秒 | error 0 |
| 同時 2 購読 (640x360@30fps) | 60 秒 | error 0 |

残る未達は 1280x720@30fps / 2000 kbps のみである (cwnd が最小値に固定され、
天井 `cwnd/srtt` が 1.6〜2 Mbps に張り付くため)。

## 検証中に見つけた別の問題: H.264/H.265 の 720p 符号化失敗 (2026-09-18)

`--video-codec h264` で 1280x720@30fps を符号化すると publisher が即座に終了する。

```text
ERROR moqt_publisher: Fatal: video toolbox: limit exceeded: plane copy dimensions exceed CVPixelBuffer plane bounds
```

- `--video-codec h265` でも同じ経路 (macOS VideoToolbox) を通るため同様の失敗が疑われる
- AV1 (既定、raden) では発生しない
- 原因はビデオツールボックスのエンコーダに渡すフレーム寸法と CVPixelBuffer の plane 境界の不整合と見られる
- 0101 (受信ループの停滞) とは独立した問題であり、別 issue として扱うべきものである

## クリーンビルドでの再検証と stale バイナリの注意 (2026-09-18)

検証は `target/debug` の publisher / subscriber を使い、relay 直結で
`moqt-publisher --fake-capture-device --width 1280 --height 720 --fps 30
--bitrate <kbps>` と `moqt-subscriber` を動かして行った。debug の subscriber は一時計測 (`DEBUG stream_slots` の
ログ出力) を含むビルドのまま残っていたため、クリーンビルドし直して再検証した
(`strings target/debug/moqt-subscriber | grep -c "DEBUG stream_slots"` が 0 であることを確認)。

| 条件 | 結果 |
| --- | --- |
| 1280x720@30fps / 1600 kbps / 120 秒 | decode error 0 / timeout 0 / PUBLISH_DONE status 4 / 3272 フレーム・5960 chunks で正常終了 |
| 1280x720@30fps / 1800 kbps / 90 秒 | decode error 0 / timeout 0 だが PUBLISH_DONE も player stop も出ず終了 (停滞) |

- しきい値は **1600〜1800 kbps** で、以前の結論がクリーンビルドでも再現した
- 一時計測はログ出力のみでロジックは同一だったが、**計測入りのバイナリで検証しない**
  という原則 (relay の stale beam 問題と同じ) を再確認した

## 同時 2 購読のクリーンビルド再検証 (2026-09-18)

クリーンビルドした debug バイナリで、同じ relay へ subscriber を 2 本同時に
接続して実行した。

| 項目 | subscriber 1 | subscriber 2 |
| --- | --- | --- |
| 映像 | 1891 フレーム | 871 フレーム |
| 音声 | 3201 chunks | 1500 chunks |
| decode error / timeout | 0 / 0 | 0 / 0 |
| 終了 | 正常終了 | 正常終了 |

relay は error 0 / warning 1。subscriber 2 のフレーム数が少ないのは起動が遅いためで、
両者とも decode error と timeout は 0 である。

## s2n-quic の受信バッチサイズを増やす案の結果 (2026-09-18)

subscriber の `Client::builder()` に Limits を渡し、`with_stream_batch_size(64)` を設定して
1280x720@30fps / 2000 kbps を 90 秒流した。

```rust
let mut limits = s2n_quic::provider::limits::Limits::default();
limits.with_stream_batch_size(64);
let client = Client::builder()
    .with_limits(limits)
    .map_err(|e| TransportError::Quic(format!("limits: {e}")))?
    .with_tls(tls)
```

| 項目 | 結果 |
| --- | --- |
| decode error | 0 |
| data stream timeout | 1 |
| PUBLISH_DONE / player stop | 出ず (停滞) |

**受信バッチサイズを増やしても停滞は解消しない** (否定材料)。`with_limits` は `Result` を
返すため `?` が必要である点も記録しておく。

また `s2n-quic-core 0.86.0` の `connection/limits.rs` を確認したところ、受信データ
ウィンドウを直接指定する setter は無く、ストリーム数の上限、`with_stream_batch_size`、
RTT / PTO 関連のみであった。

## group を小さくすると 2 Mbps でも停滞しない (2026-09-18)

`--keyframe-interval 15` (group 0.5 秒) にして 1280x720@30fps / 2000 kbps を 90 秒流した。

| 項目 | 既定 (group 2 秒) | group 0.5 秒 |
| --- | --- | --- |
| 描画 | 200〜1400 フレーム (停滞) | **2400 フレーム (26.7 fps)** |
| data stream timeout | 1 | **0** |
| relay の停滞 | 発生 | **0** |
| 音声 | — | 4400 chunks |

### 意味

制限要因は「帯域そのもの」ではなく **per-stream のデータ量 × 同時処理枠** である。

- group 2 秒では 1 本の subgroup stream が約 500 KB を運び、subscriber の 4 本の同時処理枠と
  relay の stream 上限に対して余裕が無くなる
- group 0.5 秒では 1 本あたり約 125 KB になり、同じ 2 Mbps でも stream が回り切る
- したがって「320x180@2 Mbps で停滞し、1280x720@200 kbps で安定する」という
  以前の実測 (画素数ではなくデータ量) も、object サイズと stream あたりの量で説明できる

### 実務的な回避策

publisher の `--keyframe-interval` を小さくする (既定 60 フレーム → 15〜30 フレーム) ことで
720p30/2 Mbps が安定する。relay / subscriber を変更せずに済む。
画質とのトレードオフがあるため、根本修正 (stream あたりの処理能力を上げる) は引き続き必要。

## group 長の境界 (2026-09-18)

2 Mbps で group 長を変えて比較した。

| keyframe-interval (group 長) | 描画 | data stream timeout | relay の停滞 |
| --- | --- | --- | --- |
| 60 (2 秒、既定) | 200〜1400 | 1 | あり |
| 30 (1 秒) | 1800 | 1 | あり |
| 15 (0.5 秒) | 2400 | 0 | なし |

境界は group 0.5〜1 秒の間にある (1 回ずつの実測なので実行ごとのばらつきは考慮が必要)。
1280x720@30fps / 2000 kbps を流す場合の推奨設定は `--keyframe-interval 15` である。

## group 長の緩和は 120 秒では不十分 (2026-09-18、前節の訂正)

`--keyframe-interval 15` (group 0.5 秒) で 1280x720@30fps / 2000 kbps を 120 秒流した。

| 実行時間 | 描画 | data stream timeout | relay の停滞 |
| --- | --- | --- | --- |
| 90 秒 | 2400 | 0 | なし |
| 120 秒 | 1700 | 1 | あり |

**group 0.5 秒でも 120 秒は持たない**。停滞は確率的で、group を小さくすると発生率は
下がるが解消はしない。前節の「推奨設定」という表現は誤りで、正しくは「発生率を下げる
緩和策」である。

したがって次の結論は維持される。

- 制限要因は per-stream のデータ量 × subscriber の同時処理枠である
- group 長を小さくすると 1 本あたりのデータ量が減り、停滞しにくくなる
- ただし根本原因 (cwnd が最小値に固定され、回復しない) は未解決

## マージ後の回帰確認 (2026-09-18)

relay 側の修正をマージした後の状態で、安定構成の回帰確認を行った。

| 条件 | 結果 |
| --- | --- |
| 640x360@30fps / 800 kbps / 120 秒 | 3500 フレーム (約 29 fps) / 5900 chunks / decode error 0 / timeout 0 / relay error 0 warning 0 / PUBLISH_DONE status 4 |

以前は 60 秒で確認していた構成を 120 秒に延ばしても安定している。relay の warning が 0 なのは
この構成では停滞監視自体が発動しないためで、relay 側の修正 (停滞時に保留を捨てて継続) は
通常時に影響していない。

### 現時点で実測できている安定範囲

| 条件 | 連続時間 | 結果 |
| --- | --- | --- |
| 320x180@15fps / 200 kbps | 120 秒 | error 0 |
| 640x360@30fps / 800 kbps | 120 秒 | error 0 |
| 1280x720@30fps / 1600 kbps | 120 秒 | error 0 |
| 同時 2 購読 | 60 秒 | error 0 |
| 1280x720@30fps / 2000 kbps | — | 確率的に停滞 (未達) |

## 停滞の主因は relay のパケット番号長のバグだった (2026-09-18)

subscriber (s2n-quic) に events API を入れて調べたところ、relay が送った 1-RTT パケットの
約 25% を **`DecryptionFailed`** で破棄していた。relay 側ではこれを損失として観測し、
輻輳ウィンドウが最小値に固定されて 720p30/2Mbps の恒久停滞になっていた。

### 原因 (relay 側)

`quic_connection:encode_packet_number/1` が「値が収まる最小長」でパケット番号を
エンコードしていた。RFC 9000 Section 17.1 は

> A sender MUST use a packet number size able to represent more than twice as large a range
> as the difference between the largest acknowledged packet number and the packet number
> being sent.

と定めており、損失や PTO でパケット番号だけが先行すると largest_acked との差が開き、
受信側の DecodePacketNumber が別の番号を復元して AEAD 検証に失敗する
(失敗した PN は relay の送信ログに存在しない、という実測とも一致)。

relay 自身の復号は同じ前提を共有するため EUnit では検出できず、**s2n-quic との相互接続で
初めて露見した**。relay ↔ relay のテストだけでは見つからない種類のバグである。

### 修正

relay 側の修正で、largest_acked を受け取らないこの関数は常に 4 バイトを使う
ようにし、回帰テスト (PN 長が 4 バイトであること / 受信済み PN が大きく遅れていても正しい
番号に復元できること) を追加した。

### 修正の効果 (1280x720@30fps / 2000 kbps / 120 秒)

| 指標 | 修正前 | 修正後 |
| --- | --- | --- |
| 復号失敗 | 約 25% (2593/10228) | 4.3% (1609/35548) |
| data stream timeout | 1 | 0 |
| relay の停滞終了 | あり | 0 |
| 描画 | 200〜1400 フレーム | 2400 フレーム |

### 調査で否定したもの (実測)

| 仮説 | 結果 |
| --- | --- |
| GSO / GRO / sendmmsg / recvmmsg の誤判定 | macOS の capability は `backend=gen_udp, gso=false, gro=false, sendmmsg=false, recvmmsg=false` で該当なし |
| relay の暗号化処理そのものの誤り | 全 1-RTT 経路 (`encrypt_1rtt_packet/5`) で「ヘッダ PN と nonce PN の一致」「自己復号」を検証し MISMATCH 0 / FAILED 0 |
| 反増幅による保留 | `validated=true deferred=0` |
| 送信側の宛先取り違え | UDP プロキシで relay → subscriber の全データグラムの DCID を確認し、単一の DCID で一貫 |
| 鍵更新 | relay に key phase / key update の実装が存在しない (鍵は接続生成時に固定) |

### 残っている課題

修正後も 0〜16% の `DecryptionFailed` が**回によって**発生する (0% の回もある)。
プロキシ計測では「relay が 24097 データグラム送信 → subscriber が 22685 受信 / 破棄 0」の回もあり、
発生条件は未確定。relay の送信ログと失敗 PN の対応付けは、PN 空間が 2 接続で重複するため
接続識別子 (peer port) をログに含める必要がある。

## 音声のノイズ (フレーム境界の不連続) と修正 (2026-09-18)

「音声にノイズが乗る」という報告を受け、subscriber が再生に渡す PCM をダンプして
客観評価した (440 Hz サイン波を 1 Hz で amplitude 変調した疑似音声)。

| 指標 | 修正前 | 修正後 | 期待値 |
| --- | --- | --- | --- |
| RMS | 2363 | 11913 | ≈11600 |
| peak | 6143 | 22961 | ≈22900 |
| ゼロ交差 | 940 /s | 880 /s | 880 /s |
| 440 Hz 自己相関 | 0.896 | 1.000 | 0.99 以上 |
| フレーム境界 / フレーム内の平均差分比 | **24.2** | **1.0** | 1.0 |

### 原因

`examples/moqt-subscriber/src/pipeline.rs` で **subgroup stream ごとに Opus デコーダを
生成していた**。音声は LOC draft-ietf-moq-loc-04 §4.1 に従い「1 フレーム = 1 Object =
1 Group」で届くため、20 ms ごとに新しいデコーダが作られ、フレーム間のデコーダ状態が
失われて毎フレーム位相がリセットされていた。これが 50 Hz のクリック音になり、
振幅も正しく復元されていなかった (境界の平均差分がフレーム内の 24 倍 = 波形の飛び)。

### 修正

`OpusDecoder` と Audio Config (OpusHead) の処理済みフラグを subscription で 1 つ共有する
(`Arc<tokio::sync::Mutex<OpusDecoder>>` と `Arc<AtomicBool>`)。これでデコーダ状態が
20 ms をまたいで保持される。

### 検証

- 上表のとおり振幅・周波数・連続性が期待値に一致
- 640x360@30fps / 800 kbps / 120 秒: 描画 3500 フレーム / 音声 5900 chunks /
  decode error 0 / data stream timeout 0 / relay error 0 warning 0 / PUBLISH_DONE status 4

## 断続的な復号失敗の追跡 (2026-09-18、追加計測)

PN 長の修正後も 0〜16% の `DecryptionFailed` が残るため、原因を絞り込んだ。

### 確定したこと

1. **失敗した PN は relay が実際に送ったパケットである**。relay の送信ログを Erlang ロガー
   ではなくファイルへ直接書き出し (ロガーのメッセージ欠落を排除)、subscriber の失敗 PN と
   突き合わせたところ **1975/1975 が一致**した。
   (当初「一致 0」と報告したが、これは `comm` に `sort -n` (数値順) を渡していた集計ミス。
   `comm` は辞書順を要求する。Python の集合演算で再集計して訂正した)
2. **multipath は原因ではない**。`multipath_enabled/1` を常に false に固定しても復号失敗は
   残った (1 回目 0 件 / 2 回目 568 件)。また `mpquic:negotiate/3` は本シナリオでは一度も
   呼ばれておらず、状態は生成時に `enabled=false`、ネゴシエーションは
   `LocalMaxPathId > 0 andalso PeerMaxPathId > 0` で有効化されるため、**peer が非対応なら
   multipath は使われない** (実装は正しい)。`enable_multipath/2` はアプリが明示的に呼ぶ API。
3. relay の暗号化は自己整合。全 1-RTT 経路 (`encrypt_1rtt_packet/5`) で「ヘッダ PN と
   nonce PN の一致」と自己復号を検証し MISMATCH 0 / FAILED 0。
4. macOS の capability は `backend=gen_udp` で gso/gro/sendmmsg/recvmmsg はすべて false。

### 残る仮説

relay の暗号化が自己整合である以上、peer が**別の鍵で復号している**ことになる。鍵は接続
生成時に固定され、送信経路も方向 (`Role`) で一貫しているため、残るのは **KEY_UPDATE**
(peer が鍵更新を開始し、relay が追従していない) である。relay に key phase / key update の
実装が無いことは確認済み。s2n-quic のイベント API (`PacketHeader::OneRtt`) には key phase の
フィールドが無いため、**relay 側で受信パケットの鍵フェーズビットをヘッダ保護解除後に記録**して
確認する必要がある。

## バイト整合性の確定と受信ループ停止の真因 (2026-09-18、決着)

長く追っていた「断続的な復号失敗」の決着と、それとは別に残っていた受信停止の
真因が判明したので記録する。

### バイト整合性は完全だった

relay の 1-RTT 送信経路 (`relay の 1-RTT パケット組み立て関数`) で
**組み立てた DCID と暗号文**をパケット番号ごとにファイルへ記録し、同時に
subscriber の手前の UDP proxy で S2C の実バイト列を取得して、並び順まで含めて
突き合わせた。

```text
relay 記録: 48101 行
proxy S2C:  11976 パケット
  DCID 98c1791ddd2f4d25: relay 側 11967 / proxy 側 11967
    完全一致 11967 / 同長で中身不一致 0 / relay のみ 0 / proxy のみ 0
  DCID aa1e32d584f3c2e2: relay 側 6 / proxy 側 6 → 完全一致 6
=== 合計 完全一致 11973 / 中身不一致 0 ===
```

relay の PN は 6〜11972 が**欠番も重複もない連番**で、再送は 0 件だった。

これにより次が確定する。

- **relay の送信バイト列は正しい**。暗号化・ヘッダ保護・ノンスのいずれも正しい
- **経路 (loopback の UDP) で 1 バイトも化けていない**。proxy が観測した全パケットが
  relay の記録と一致した
- したがって「peer が別の鍵で復号している」という仮説 (KEY_UPDATE 説) は**否定**される。
  バイト列が同じで鍵も同じなら、復号は必ず成功する

以前「1975/1975 が一致」と記録した計測は `comm` に数値順ソートを渡していた集計ミス
だったが、今回は並び順まで含めた全数一致で確定した。

なお、この計測の途中で hex 文字列の切り出し位置を 1 文字ずらしていたため
「一致 0」という誤った中間結果を出した。hex は 1 バイト = 2 文字であり、
バイト位置 n は 2n 文字目から切り出す必要がある。

### 受信停止の真因: select! の分岐の中で permit を待っていた

`examples/moqt-subscriber/src/pipeline.rs` の受信ループは、data stream の accept と
`client.next_event()` を 1 つの `tokio::select!` に並べていた。accept した直後に
**分岐の中で** `stream_slots.acquire_owned().await` していたため、stream 枠 (4 本) が
埋まると次の accept で分岐の中に留まり、同じ `select!` にある `client.next_event()` が
poll されなくなる。

s2n-quic は `next_event()` の poll でエンドポイントを駆動する。ここが止まると

- 受信したパケットを処理できない
- ACK を送れない
- タイマー (損失検出・PTO) が動かない

が同時に起き、relay 側から見ると無応答になる。relay の輻輳ウィンドウが最小値へ落ちて
配送が止まり、30 秒後に subscriber が `0x12 data stream timeout expired` で終了する。

これが「約 45 秒で止まる」の正体だった。枠の取得を `select!` の外へ出し、accept した
stream を一旦キューへ積んでから空き枠の分だけ処理へ回す形に変更した。キューが
`MAX_CONCURRENT_STREAMS * 4` を超えたら accept 自体を止め、s2n-quic に stream を
保持させる。

### 併せて直した点: ピアが開ける stream 数の上限

s2n-quic の既定はピアが開ける単方向 stream が累積 100 本。本 example は音声を
20ms ごと、映像を 1 frame ごとに 1 本の stream で受けるため、消費が回復を上回る。
`with_limits` で上限とウィンドウを明示した。

### 実測 (relay 直結、SIGSTOP なし、各 120 秒)

```text
640x360@30   800kbps   Rendered 3300 / Played 5700  完走
1280x720@30 1200kbps   Rendered 3300 / Played 5700  完走
1280x720@30 1600kbps   Rendered 3400 / Played 5700  完走
1280x720@30 2000kbps   Rendered 1200 で停止 (改善、未完走)
```

relay 側の error / warning は全条件で 0 件。修正前は 720p/2Mbps で 200 frames
止まりだったので、同じ条件で 6 倍になった。

### 副次的に判明した点

- relay の既定輻輳制御が **NewReno (損失ベース)**。メディア中継には BBR v3 が適切で、
  切り替えると停止位置が 22 秒 → 52 秒へ伸びた
- 「フロー制御で送信が捨てられる」問題は本シナリオでは**発生していない**
  (`send_error=0`、`flow_control_blocked` 0 件)
- ただし relay は `_ = relay の送信関数(...)` と戻り値を捨てている箇所が 8 か所あり、
  失敗が黙って消える構造になっている。ここは別途対処が必要

### relay 側の計測で分かったこと

- `upstream_objects` は停止後も毎秒 80 件で増え続ける。relay は上流を読み続けている
- `send_ok` も毎秒 80 件で増え続ける。relay は下流へ書き続けている
- `pending` は常に空、`stalled` は 0
- したがって停止は relay ではなく subscriber 側の受信ループにある (上記の真因と一致)

### 残る課題

1280x720@30 / 2000 kbps だけが 120 秒完走しない。1600 kbps は完走するため、
1600〜2000 kbps の間に境界がある。次は停止直前の relay の輻輳ウィンドウと
subscriber の受信レートを同時に記録して、帯域不足なのか別の停止なのかを切り分ける。

## 残課題: 2000 kbps だけが 120 秒完走しない (2026-09-18)

受信ループの修正後、640x360@30/800kbps、1280x720@30/1200kbps、1280x720@30/1600kbps は
120 秒完走するようになったが、1280x720@30/2000kbps だけは 40〜60 秒で止まる。
1600 kbps は完走するため、1600〜2000 kbps の間に境界がある。

### 分かったこと (すべて実測)

止まった時点で次を確認した。

1. **relay は上流を読み続けている**。relay の heartbeat で `upstream_objects` が
   止まったあとも毎秒 80 件で増え続ける
2. **relay は下流へ書き続けている**。`send_ok` も毎秒 80 件で増え続け、
   `send_error` は 0、`flow_control_blocked` も 0 件
3. **relay の fan-out に滞留は無い**。`pending` は常に空、`stalled` は 0
4. **subscriber の受信ループは生きている**。`free_slots=4` のまま
   `accept_recv_stream()` が新しい stream を返さない
5. **subscriber のプレイヤーループも生きている**。`PLAYERDIAG` が 1 秒ごとに
   回り続け、`backlog=0` のまま frames / audio が増えない
6. **映像と音声が同時に止まる**。トラック単位ではなく接続単位の事象
7. **CPU が決定的な証拠**。停止の瞬間に subscriber が 10% → 1% へ落ち、
   同時に relay が 6% → 100% 超へ跳ね上がる
8. **wire のバイト速度が階段状に落ちる**。640 KB/s で推移していたものが、
   停止の直前 2 秒だけ 2200〜3930 KB/s に跳ね、以後 190 KB/s で一定になる
9. **止まったあとのパケットは 500 バイト帯だけ**。停止前は 300〜400 バイト帯と
   1500 バイト (MTU 満杯) が混在していたが、停止後は 500 バイト帯のみになる。
   輻輳ウィンドウが縮んで MTU 満杯のパケットを出せなくなった状態と整合する

### 現時点の判断

subscriber 側の受信ループは生きていて accept も呼ばれているのに新しい stream が
来ない。relay 側は送信に成功し続けている。したがって止まっているのは
**subscriber の s2n-quic 受信経路**であり、アプリケーション層の処理ではない。

relay の輻輳ウィンドウが最小値近くまで落ちて復帰しない点は relay 側の
輻輳制御にも関係する。relay 側の課題 (bbr_v3 の in-flight 会計) と併せて追う。

### 決定的な切り分け: 停滞は bitrate 依存で fps 依存ではない (2026-09-18)

同じ 2000 kbps のまま fps だけ 30 から 15 へ落として 120 秒動かした。

```text
1280x720@30 / 2000kbps: Rendered 1300 で停止
1280x720@15 / 2000kbps: Rendered 1081 で停止 (総フレーム数が半分でも同じく停止)
```

**どちらも停止する。** 停止するかどうかを決めるのは 1 秒あたりのバイト数であって、
フレーム数でも 1 フレームあたりの大きさでもない。したがってこれは
アプリケーション層の処理量の問題ではなく、**転送レートの上限**である。

**ただしこの「輻輳ウィンドウが上限」という見方は後に実測で否定された。**
下の「訂正」を参照すること。

### 訂正 (2026-09-18): レート上限ではなく、再送バースト後の状態破壊である

relay の既定を bbr_v3 にしても 2 Mbps の停滞は改善せず (Rendered 1300 -> 643)、
tcpdump で relay -> subscriber のレートを測ると上限に張り付いていなかった。

```text
1600 kbps 設定: 645〜655 pps で安定、実測 3.55 Mbps を 60 秒維持 (Rendered 1800)
2000 kbps 設定: 736〜745 pps で推移 -> t=45 秒に 1217 pps へ跳ねる
                -> 以後 426 / 331 / 264 pps へ低下して回復しない (Rendered 1300)
```

1600 kbps 設定では目標 1.6 Mbps に対して実測 3.55 Mbps 出ており、
輻輳ウィンドウ由来の上限には達していない。2000 kbps 設定でも停止前は
736 pps (4 Mbps 相当) で送れている。

したがって 2 Mbps の停滞は**レート上限ではなく、t=45 秒前後の再送バーストを
契機に送信側か受信側が回復不能な状態へ入る不具合**である。cwnd の修正では
解消しない。relay 側の課題に残課題として記録した。

### 次にやること

- t=45 秒の再送バーストの契機を特定する。どのパケットが損失判定され、何が
  再送されたのかを relay 側で記録する
- バースト後にレートが 250〜340 pps へ落ちて回復しない理由を特定する。
  送信側の pending が詰まるのか、受信側が ACK を返さなくなるのかを切り分ける
- 1600 kbps 設定で同じ跳ねが起きない理由を特定する (データ量の差か、
  フレームサイズの差か)

## relay 側からの再測定 (2026-09-18): QUIC エンドポイントが polling されなくなる

relay 側で 1280x720@30 / 2000 kbps、130 秒の再測定を行い、tcpdump (loopback
両方向) と本 issue で追加した `MOQT_PACKET_DIAG` を同時に取った。relay 側の
「relay は引き金ではなく被害者である」という結論の根拠である。

### 順序

```text
t=50706ms STREAMDIAG total_streams=2558 free_slots=1 active_tasks=2558
t=50806ms STREAMDIAG total_streams=2558 free_slots=4 active_tasks=2558
          ^ 新規 stream の accept が止まる (3 スロットが空いたまま埋まらない)
t≈51000ms PKTDIAG received=20287 dropped=116 decryption_failed=116
          ^ 以後 received は 130 秒の実行が終わるまで 20287 のまま動かない
```

- `received` が増えないので、**relay が送ったパケットは s2n-quic に届いていない**
- 同時に 116 件の `decryption_failed` が出る。以降は 1 件も増えない
- カーネルの `netstat -s -p udp` の "dropped due to full socket buffers" は
  0 件のまま (594 → 594) なので、OS のソケットバッファ溢れではない
- この時点で `STREAMDIAG` と `PLAYERDIAG` の 100 ms 周期ログは動き続ける。
  プレイヤーループと診断タスクは生きている
- relay は同方向へ毎秒約 420 パケットで送り続けており (再送バーストではない)、
  その後 ACK が来ないため輻輳ウィンドウが 23.9MB から崩壊する

`PLAYERDIAG` は `backlog=0` のままなので、表示側の backpressure ではない。

### 見えていること

- subscriber のメインループが新しい stream を accept しなくなり、続いて
  QUIC エンドポイントが polling されなくなっている
- `active_tasks` は 2558 で固定され、既存タスクは完了も増加もしない
- `free_slots` が 1 から 4 へ増えているので、permit は 3 件返却されている

次に確認するのは、`accept_recv_stream()` が呼ばれなくなった理由と、
`client.next_event()` が poll されなくなった理由である。両者が同時に止まるため、
`pipeline.rs` の `'main: loop` がどこかで待機したままになっている疑いが強い。

## 前提の訂正と停止機構の候補 (2026-09-19、コード読解)

s2n-quic の IO 実装を読んで、これまでの前提が 2 つ誤っていることが分かった。

### 訂正 1: `next_event()` の poll はエンドポイントを駆動しない

`.with_io("0.0.0.0:0")` は `s2n_quic_platform::io::tokio::Io` に解決され、その `Io::start` が
`task::rx` / `task::tx` / `EventLoop::start` の 3 タスクを **tokio runtime に spawn** する。
パケット受信も ACK 送信もタイマーも EventLoop タスクが回す。

したがって「`next_event()` が poll されない → エンドポイントが駆動されない → ACK が
止まる」という因果は成立しない。正しい問いは
**「spawn された EventLoop / rx タスクがなぜ止まったのか」**である。
`PKTDIAG` の `received` 凍結は「IO タスクが動いていない」ことを意味する。

### 訂正 2: `STREAMDIAG` の `free_slots` は使用中の数である

`free_slots = MAX_CONCURRENT_STREAMS.saturating_sub(stream_slots.available_permits())`
なので **使用中** permit 数である。`free_slots=4` は「4 本が枠を握ったまま」を意味する。
また `active_tasks = join_set.len()` は `join_next()` を呼ぶまで完了タスクが残るため
`total_streams` と常に一致し、「実行中のタスク数」ではない。

### accept が止まる条件は 3 状態だけ

1. `accepting == false` (`pending_streams.len() >= MAX_CONCURRENT_STREAMS * 4` = 16)。
   `select!` の `if` ガードで分岐ごとスキップされる。これが「ループは生きているのに
   accept が呼ばれない」唯一の経路
2. 呼ばれるが Pending (トランスポートが新しい stream を返さない)
3. `Ok(None)` で `break 'main`。今回は起きていない

`STREAMDIAG` が出続けている以上ループは回っており、`tick_interval` が成立している。

### permit は stream の終端まで保持される

`spawn_stream_task` は FIN / RESET で stream が終わるまで permit を離さない。終わらない
stream が 4 本あると permit が返らず、`pending_streams` が 16 で accept が無効化される。
既存の実測 (最長 62 秒の stream、`available_permits=0` 固定) と一致する。
音声は `decoder.lock().await` を decode 全体 (ネットワーク await を含む) で保持するため、
permit と mutex の二重待ちも起きる。

### QUIC 側が沈黙する条件 (確度順)

`handle_short_packet` は Closing / 復号失敗などのどの経路でも `on_packet_dropped` を出し、
復号成功時に `on_packet_received` を出す。**`received` と `dropped` が両方凍結している
ことは「接続に datagram が 1 つも渡っていない」ことを意味する** (届いて復号に失敗して
いるのではない)。

1. **EventLoop タスクが誰にも通知せず return した**。`EventLoop::start` は select / rx / tx の
   エラーで黙って return し、`Rx::handle_error` はイベントを出さない。rx タスクはソケット
   syscall のエラー 1 回で終了する。`Io::start` の `JoinHandle` は誰も見ていないため
   panic も return も不可視
2. **`DatagramDropReason::UnknownDestinationConnectionId` で endpoint が捨てている**。
   この経路は endpoint レベルのイベントしか出ないため、接続レベルの
   `received` / `dropped` は両方凍結する
3. runtime 飢餓 (CPU 1% と矛盾)、rx の lost wakeup (カーネル drop 0 と整合しにくい)

### 次にやる計測 (決定打)

`PacketDiag` に次を足して 1 秒ごとに集計する。

- `on_platform_event_loop_started` / `on_platform_event_loop_wakeup` / `on_platform_event_loop_sleep`
  → EventLoop タスクの生死
- `on_platform_rx` (`count` / `syscalls` / `blocked_syscalls` / `total_errors` / `dropped_errors`)
  → rx 経路がソケットを読んでいるか
- `on_platform_rx_error { errno }` / `on_platform_tx_error { errno }` → 致命エラーの errno
- `on_endpoint_datagram_dropped` (reason 別) → 候補 2 の確定
- `on_connection_closed { error }` → 接続が閉じた理由

判定は、platform event loop が止まる = 候補 1 / wakeup は続くが
`UnknownDestinationConnectionId` が出る = 候補 2 / `on_platform_rx.count` は増えるが
`received` が凍結 = ルーティング以前で捨てられている。

### 修正案 (設計)

1. **permit を stream の終端まで保持しない**。受理は即座に行い、デコード同時数だけを別
   semaphore で制限し、枠が取れなければ `STOP_SENDING` で捨てる。終わらない stream は
   時間で打ち切る
2. `accepting == false` の間は必ず警告を出し、`select!` に `else` を足して「全分岐無効」を
   検出可能にする (現状は完全に無症状)
3. **endpoint の死活監視**。platform event loop の wakeup を監視し、止まったらログ + 終了。
   あるいは IO provider を自前実装して `JoinHandle` を監視する
4. `next_event()` を `select!` の分岐に直接置かず、専用タスク + mpsc に移す。現状は
   cancel safe でなく、敗者になったときに送信途中の `Bytes` が drop されて制御ストリームに
   穴が空きうる

## 最終的な原因: relay が経路 MTU を超えるパケットを送っていた (2026-09-18)

relay 側で relay の送信 1 パケットごとのサイズと、subscriber の
`decryption_failed` を同時に計測して確定した。

```text
relay -> subscriber の送信パケット
  <= 1500 バイト: 12632 件
  >  1500 バイト:   121 件   最初 pn=18478 (size=1501) 最後 pn=18615 (size=1513)

subscriber の復号失敗
  decryption_failed=121     失敗した pn は 18478〜18497, ..., 18615
```

1500 バイト超の送信 121 件と復号失敗 121 件は **1 対 1 で一致する**。別の実行でも
1500 バイト超 68 件に対して復号失敗 130 件と同じ相関が出た。

### なぜ復号に失敗するか

s2n-quic の経路 MTU の上限は 1500 バイトで固定である。

```text
s2n-quic-core-0.88.0/src/path/mtu.rs:222
const DEFAULT_MAX_MTU: MaxMtu = MaxMtu(NonZeroU16::new(1500).unwrap());
```

1500 バイトを超える datagram は受信時に切り詰められる。ヘッダー保護は外せるが
AEAD の入力が途中で切れるため、`PacketDropReason::DecryptionFailed` になる
(`UnprotectFailed` ではないことが切り詰めの証拠である)。

### 止まるまでの連鎖

1. relay が 1500 バイト超のパケットを送る
2. subscriber は復号できず ACK を返さない (RFC 9001 Section 6.6 の範囲内で正しい)
3. relay は時間しきい値の損失検出で輻輳ウィンドウを最小値まで落とす
4. relay は PTO プローブしか送らなくなる
5. subscriber には ACK を返す対象が無く、両者が止まる
6. `subscriber_stall_timeout` (既定 30 秒) で relay が subscriber を切断する

relay が 1500 バイト超を送っていたのは、ピギーバックする ACK フレームの
サイズを勘定に入れていなかったためである。ACK フレームは 521〜530 バイトあり、
1109 バイトの STREAM フレームに載せると 1516 バイトになった。

### 以前の計測の訂正

「停滞中も relay は同方向へ毎秒約 420 パケット送り続けている」と記録していたが、
これは relay のもう 1 つの接続 (publisher 向けの ACK) を数えていた。subscriber 接続の
送信は停滞時に停止している (`send_pn` は 18616 で止まり、以後増えない)。

### 修正

relay 側 (relay) を修正した。moqt-rs 側の修正は不要である。

- relay 側の修正: 1-RTT パケットの上限を経路 MTU とピアの広告値の小さい方にし、
  ピギーバックする ACK を残り容量に収める (relay 側の修正)
- relay 側の修正: ACK 範囲の記憶数を制限し、ACK フレームを小さくする (relay 側の修正)

### 修正の検証 (1280x720@30fps / 2000 kbps / 120 秒)

| 項目 | 修正前 | 修正後 |
| --- | --- | --- |
| relay の送信最大サイズ | 1516 バイト | 1223 バイト |
| 1500 バイト超の送信 | 121 件 | 0 件 |
| subscriber の復号失敗 | 121 件 | 0 件 |
| 描画フレーム数 | 643〜1700 で停止 | 3600 (120 秒完走) |
| subscriber の RSS | 134〜156 MB | 115〜138 MB |
| subscriber の CPU | 9〜11% | 11% |

メモリ修正 (tokio-metrics) と本修正で、0101 の停滞は解消した。

## 解決方法

1. tokio-metrics の `intervals().collect()` を修正し、メモリ爆発と CPU 飽和を解消した
2. stream の permit を `select!` の枝の外で取るようにし、枠待ちで
   `client.next_event()` が poll されなくなる構造を解消した
3. relay の経路 MTU 超過を relay 側で修正した
