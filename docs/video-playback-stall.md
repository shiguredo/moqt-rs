# moq-sub の映像描画が停止する問題の原因と修正

- 調査日: 2026-10-21
- 対象: moqt-rs の `examples/moq-sub`、raw-player-rs の `VideoPlayer`

## 症状

moq-pub のフェイクキャプチャ映像を moq-sub で受信すると、受信とデコードが続いているのに
映像の描画が長時間止まった。1 回の試験では、プレイヤーへの投入 2369 枚に対して描画 11 枚、
破棄 2353 枚、キュー残 5 枚だった。既存の `Rendered ... video frames` ログは投入のたびに
増えており、描画が止まっていることを示していなかった。

## 原因

### raw_player の映像のみ再生の時計

raw_player の `VideoPlayer` は、音声が開始していない映像のみ再生では、最初に描画処理へ
入った時点で**キュー先頭フレームの PTS を SDL の単調時計へ一度だけ対応付け**、以後はその
対応を固定した時計で表示時刻を判定する。判定幅は `sync_threshold_us` (40 ms) で、それより
早いフレームは待ち、遅いフレームは捨てる。

このため、アプリ側の投入タイミングが時計から大きくずれると、時計と到着 PTS の差が固定
されたままになり、全フレームが判定幅の外へ出て描画が止まる。キュー上限は 5 枚であり、
「早すぎる」側では表示時刻に近いフレームが上限超過で先に捨てられ、まだ先の時刻のフレーム
だけが残る。逆にアプリ側の処理停滞で時計だけが進むと、以降の全フレームが遅延ドロップされ
続ける。

再現は実 SDL (dummy ドライバ) の単体試験で確認した。PTS に試験開始からの経過時間を使い、
最初のフレーム投入から最初の `poll_events()` までだけ 300 ms 待つと、最初の 1 枚を描画
した後に約 8 秒間描画が進まなかった。

### moq-sub の会計

moq-sub は、プレイヤーへ `enqueue_video_i420` した時点で描画数 (`frame_count`) を増やし、
`PlayoutTimeline::record_presentation` にも投入時刻を記録していた。実際の描画は
`poll_events()` の中で起きるため、描画が止まっていてもログと表示実績だけが進んでいた。

## 修正

### raw-player-rs

- 映像のみ経路で描画できない状態が `RESYNC_TIMEOUT_US` (1 秒) 継続したら、キューの最新
  1 枚以外を捨て、そのフレームの PTS に再生時計を再基準化して即座に描画する
  (`resync_to_latest_frame`)。
- `blocked_since_ns` は「遅延ドロップした」「早すぎて待った」ときに設定し、描画できたら
  消す。フレーム未到着の通常の待機では設定しない。pause / play / stop / drain_video では
  クリアし、pause 中に進んだ時間で誤って復帰しないようにする。
- 未来の PTS が正当な先行バッファである場合に誤発動しないよう、復帰は「描画できない状態
  の継続時間」だけで判定する。音声が開始している経路では音声時計を正とし、時計の再基準化
  はしない。
- `VideoPlayerStats` に理由別ドロップ数 (`dropped_frames_queue_full` / `dropped_frames_late`
  / `dropped_frames_resync`) と `resync_count` を追加した。`dropped_frames` は合計として
  維持する。

### moq-sub

- `poll_events()` の直後に `stats()` を読み、描画数・破棄数の差分から実際に描画された
  フレームを求める。投入したフレームの TIMESTAMP は FIFO で対応付け、描画されたものだけを
  その観測時刻で `PlayoutTimeline::record_presentation` に記録する。TIMESTAMP の無い
  フレームは位置合わせのため `None` として FIFO に積み、記録からは除く。
- 投入時のキュー上限超過は FIFO の先頭を同時に除いておき、次回の破棄差分で二重に除かない
  ようにする。
- 描画が 1 秒以上進まず、その間に 2 枚以上投入している場合は `drain_video()` で復帰し、
  警告ログと復帰回数を出す。raw_player 2026.3 は自力で復帰するが、それより前の版でも
  描画を再開できるようにする保険である。
- ログを実測値に変えた。毎秒の `Video player: enqueued=... rendered=... dropped=...
  (queue_full=... late=... resync=...) queue=... drain_recoveries=... player_resyncs=...`
  と、終了時の内訳がこれにあたる。投入数を描画数として出す `Rendered ... video frames`
  は削除した。`drain_recoveries` は moq-sub 側の復帰、`player_resyncs` は raw_player 側の
  復帰の回数である。

## 検証

- raw-player-rs: 実 SDL の単体試験で、時計が PTS より遅れる場合・進む場合の両方から
  復帰して描画が継続すること、正常な 30 fps 再生では復帰が発動しないこと、理由別ドロップ
  数が合計と一致することを確認した。修正前のコードでは復帰の 2 ケースが描画 1 枚で
  失敗することも確認している。
- moq-sub: `VideoPlayoutAccounting` の単体試験で、描画・破棄・キュー上限超過の対応付け、
  停止判定の境界、復帰後のリセットを確認した。`cargo clippy -p moq-sub --all-targets --
  -D warnings` と `cargo test -p moq-sub` が通る。

## 残課題

- raw_player の修正は 2026.3.0-canary.1 としてリリースされ、moqt-rs の依存も更新した。
  moq-sub の `drain_video` による復帰は、それより前のプレイヤー向けの保険として残す。
- 実 MoQ リレーを介した 90 秒以上の送受信で、投入数だけでなく描画数が増え続けることの
  確認は未実施である。
- 表示時刻を moq-sub が決め、raw_player がさらに別の時計で判定する二重管理は残っている。
  raw_player に「選択済みのフレームを PTS 待ちなしで描画する」API を足して責務を
  moq-sub へ寄せる案は、別途扱う。
