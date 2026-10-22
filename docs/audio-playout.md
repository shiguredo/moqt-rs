# 音声 playout の移植と比較見直し

2026-10-20 に「qmux 経由で音声がプチプチする」という報告を受けて playout を moqt-js と
比較し、2026-10-22 に moqt-js の 0879〜0883 (音声だけの購読の遅延、穴 1 個での上限拡大、
A/V 同期の再設計) を移植した記録である。原因の整理と、実施した対策・移植した範囲の根拠を
残す。数値は特に断らない限り moqt-js 側の実リレー計測による。moqt-rs の実機計測はまだ行って
いない。

## 2026-10-20 の見直し (qmux のプチプチ)

### 結論

- moqt-js の playout の純粋な計算 (時間圧縮、補間、遅延推定、閉ループ、追いつき、時間軸) は
  行単位で移植されており、数式・定数・境界に聴感差を生む差異は無かった。
- プチプチの主因は移植ミスではなく、再生方針と出力経路にあった。moqt-js 側でも同じ問題
  (補間しなかった隙間が境界の段差になる、予約できない隙間がアンダーランになる) が実測で
  確認されており、moqt-rs は出力が FIFO のため、隙間が無音としても残らず再生が早期化する
  分だけ悪化し得る。
- qmux は原因を増幅する。QMux の DATAGRAM は TCP 1 本の順序付きストリームに載るため、
  TCP の再送待ちで音声がまとめて遅れて届く。受信バッファの上限を超えると破棄される。

### 一致していた点

- 時間圧縮・補間: `src/playout/stretch.rs` と moqt-js の `audioTimeStretch.ts` は、4 kHz への
  間引き、相関窓、ピッチ探索、クロスフェード、放物線補間、継ぎ目の段差判定、末尾のフェード
  まで一致する。
- 再生スケジューラ: `src/playout/scheduler.rs` と `audioPlayout.ts` は、目標時刻の決定、
  並べすぎの判定、到着基準へのフォールバック、詰める量の式、隙間の条件が一致する。
- 遅延推定・閉ループ・追いつき: `delay.rs` / `feedback.rs` / `catch_up.rs` は、窓・分位点・
  忘却・上限・増減の速さ・床・cooldown まで一致する。
- 時間軸: `timeline.rs` と `timeline.ts` は、基準の 10 秒窓最小値、A/V 同期の不感帯、
  補償上限、ドリフト判定、減衰まで一致する。

### 対策 (2026-10-20)

- 再生の隙間を必ず材料化する。スケジューラは隙間の全長と補間してよい長さを分けて返し、
  呼び出し側は補間できなかった分をフェード付きの無音で埋める。隙間の前後は 2 ms で 0 に
  落として段差を消す (`src/playout/gap.rs`)。
- 復号した音声はその場で積み、最初の目標時刻に達してから再生を開始する。これにより目標
  遅延ぶんの先行バッファが蓄積される。映像の vsync は無効にする。
- 出力が枯れたらその量を目標遅延の閉ループへ入力する。アンダーランは事後には埋められない
  ため、目標を上げる以外に減らせない。
- 受信した音声の到着時刻と復号完了時刻をフレームへ載せ、時間軸への観測には復号完了時刻を
  使う。datagram の sleep は空バッチのときだけにし、音声は毎ループ全件取り出す。
- 映像 jitter の上限超過サンプルの除外と、基準の共有解除の往復検出を移植する。

## 2026-10-22 の追加移植 (moqt-js 0879〜0883)

### 0879 目標遅延の内訳

- `PlayoutTimeline::audio_delay_feedback_stats()` を追加した。音声だけを購読するときは A/V
  同期の推定を出さないため、閉ループが決めた目標遅延の内訳をここから読めるようにする。
- 閉ループの snapshot に、揺らぎだけから求めた目標 (`jitter_target_us`) と実際に適用する値
  (`applied_us`) を追加した。

### 0881 音声だけの到着基準再生の遅れの下限

- `TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US` (140 ms) を追加した。音声だけを購読すると
  映像に合わせる相手が無く到着基準で並べるが、閉ループの適用上限が 100 ms のため、Wi-Fi の
  周期的な停滞 (約 0.5 秒ごとに 100 ms 前後) を吸収しきれなかった。
- moqt-js の実リレー Wi-Fi 計測の A/B (0 / 120 / 140 ms を各 6 回、60 秒) で 140 ms を採用
  した。0 ms は 6 回すべてで無音の穴 (最大 116 ms、復旧できない分 28〜43 ms) と境界の段差が
  出た。120 ms は穴が 1〜12 ms まで縮んだが 6 回すべてで境界の段差が 1 回出た。140 ms は
  4 回で穴も段差も 0、残る 2 回は経路が 140 ms を超えて跳ねた回で 33 ms の穴 (復旧できない
  分 16〜17 ms) が 1 回だけであった。
- 映像を購読しているときはこの下限を渡さない (A/V のずれになる)。

### 0882 穴 1 個での上限拡大と、音声だけの上限

- `AudioDelayFeedback` に穴 1 個で一時的に広げる適用上限 (`gap_ceiling_us`) を追加した。穴は
  事後には埋められないため、1 個でも「経路の揺らぎが目標を超えた」証拠として扱う。
- 穴の長さ + 余白 (20 ms) を吸収できる値まで適用上限を広げ、最後の穴から
  `AUDIO_DELAY_FEEDBACK_GAP_QUIET_US` (5 秒) の間だけ保持する。保持中も目標を上げる向き
  (遅れ・並べすぎ・穴) は許すが、下げる向きへは進まない。
- 広げる値は `AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US` (160 ms) と明示設定
  (`targetLatency`) の小さい方で切る。既存の artifact ceiling (圧縮か穴が 2 秒続いたときに
  160 ms へ広げる規則) は残し、穴 1 個の一時的な広げと段階を分ける。
- `TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US` (160 ms) と
  `audio_only_arrival_delay_us(learned_delay_us, floor_us)` を追加した。音声だけの購読では
  下限 140 ms を確保したうえで、閉ループが穴で広げた目標を 160 ms まで使う。共有の基準が
  あるトラックの上限 (`TIMELINE_ARRIVAL_DELAY_US` の 100 ms) は変えない。
- `AudioDelayFeedbackObservation` に、累積の穴の数と長さ (`gap_count` / `gap_us`) と、
  音声だけの到着基準の下限 (`arrival_delay_floor_us`) を追加した。

### 0883 A/V 同期の再設計

- 同期が足す前の表示時刻から、共有する表示時刻の下限を 1 つだけ決めるようにした。必要値が
  増えたら即座に上げ、減ったら毎秒 `TIMELINE_DELAY_DECAY_US_PER_SECOND` (20 ms) までで
  戻す。過去に各トラックへ足した補償を必要値の算定へ戻さないため、揺らぎが上下しても待ちが
  積み上がらない。
- 各トラックの補償は共有の下限から毎回計算し直す。自分の jitter buffer が補償を埋めた分は
  差し引く (表示時刻を前へ動かさずに余分な補償を解消できる)。
- 基準の差には `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` (100 ms) の保護を掛け、同じ向きの
  既知のバッファ差は別に補償する (`sync_compensation_limit_us`)。先行側は両方を合成した
  実際の表示時刻から決める。
- 予約した時点の予定と実際に出力された時刻の差 (残差) を 250 ms の時定数で平滑化し、映像の
  合わせ先へ足す (`record_presentation_with_target`)。音声の予測 (予定どうし) だけを揃えると、
  実際に鳴る音から映像が先行したままになる。残差は
  `DelayBreakdown::audio_playout_correction_us` で読める。有効な出力の観測が 1 秒途切れたら
  残差を失効させる。
- 閉ループの開始値を安全下限 (50 ms) にした。最初の再生の観測だけで 100 ms を適用すると、
  静かな経路でも映像が跳ねて遅れる。経路と出力に必要な分は観測から増やす。
- `AudioDelayFeedback::reset()` を追加し、音声の再生を取り直したら過去の穴と遅れを持ち込ま
  ないようにした。`PlayoutTimeline::reset_track` / `reset` から呼ぶ。
- 目標を広げた呼び出しの中で同期も更新する。次の復号出力まで待つと、穴からの復帰時にその間
  だけ音声と映像の時刻が離れる。
- 音声の観測が無く再生の観測だけがある場合も、閉ループの目標を到着基準の遅れに使う
  (`playout_delay_us` のフォールバック)。
- `targetLatency` を設定した時点で、実際に使う音声の遅れも更新する。次の観測を待つと、停止
  中などでは計器だけが新しい上限になり、実際の保持値には古い目標が残る。

### 周期探索の修正 (audioTimeStretch.ts)

- 補間の末尾周期の探索で、細探索の範囲を 1 サンプル外側まで広げ、放物線補間に必要な隣接点を
  実際に評価するようにした。探索範囲の端に最大があるときに、評価していないラグで外挿して
  周期が 0.5 サンプルずれる問題を直す。
- 整数ラグが丸め誤差の範囲で完全相関のときは、放物線の頂点へずらさない。有限窓の前後の相関は
  非対称になり得るため、正しい整数周期を頂点へずらすと位相を壊す。

## 移植の範囲

### moqt-rs に移植したもの

- `src/playout/timeline.rs`: 基準の学習、表示の遅れ、A/V 同期 (共有の表示時刻の下限、補償
  上限、出力残差)、基準を共有しない判定と解除、遅延の内訳。
- `src/playout/feedback.rs`: 実際に鳴った結果からの目標遅延の閉ループ。穴 1 個での上限拡大と
  5 秒保持、開始値 50 ms、再開時の初期化。
- `src/playout/timing.rs`: 再生の観測と計器。制御に使う分布の開始時刻 (再開前の実績を除く)
  と、穴の累積。
- `src/playout/delay.rs`: 到着の遅れの分布からの目標遅延の学習。
- `src/playout/stretch.rs`: 時間圧縮と補間 (周期探索の修正を含む)。
- `src/playout/scheduler.rs` / `gap.rs` / `splice_fade.rs` / `catch_up.rs` /
  `publish_catch_up.rs`: 鳴らす時刻・隙間の材料化・フェード・追いつき。
- `examples/moq-sub`: 音声だけの購読の判定と到着基準の遅れの下限、出力残差の記録、閉ループ
  への観測。

### 移植対象外 (ブラウザ固有)

- Web Audio (`AudioContext`、`AudioBufferSourceNode`、`AudioClockBridge`)、WebCodecs
  (`AudioData` / `AudioDecoder`)、`HTMLAudioElement`、devtools。
- moqt-js の `preDecodeDelay` / `decodeInputTimestamps` / `timedValues` /
  `videoPlayoutSession` は高レベル API とブラウザのメディア要素に依存する。moqt-rs には
  相当する層が無いため移植しない。
- 44.1 kHz の解析用リサンプル。moqt-rs の example は Opus 48 kHz 前提であり、44.1 kHz の
  音声では時間圧縮も補間も働かない (`stretch::is_time_stretch_available` が false)。

## 既知の差異と限界

- **穴の計器の意味が moqt-js と違う。** moqt-rs の `gaps` / `gap_total_us` は材料化した隙間
  の全長であり、補間で埋めた分も含む。moqt-js の `gapCount` / `gapMs` は補間で埋まらなかった
  分だけである。閉ループの穴による上限拡大は、moqt-rs のほうが敏感に反応し得る。実リレーの
  計測で広がりすぎるようなら、補間で埋まらなかった分だけを数える計器を足す。
- **出力残差の意味が moqt-js と違う。** moqt-js は `AudioContext` の予約時刻との差を使い、
  下流の `HTMLAudioElement` やデバイスが足す遅延は含まない。moqt-rs は `raw_player` の実際の
  再生位置から求めるため、プレイヤーの内部バッファを含む。実際に聞こえる音に映像を合わせる
  目的では moqt-rs の値が正しいが、値の大きさは moqt-js と単純には比較できない。
- **44.1 kHz の音声 (AAC) は時間圧縮も補間も働かない。** Opus 48 kHz の購読を前提にする。
  macOS の AAC 44.1 kHz の再生では `stretch_unavailable_frames` が増える。主要経路
  (Opus 48 kHz) と録画 (Opus のみ) には影響しない。
- **実リレーでの計測はまだ行っていない。** 以下の数値は moqt-js 側の計測に基づく。

## 検証方法

- 単体・統合・PBT:
  - `cargo test -p shiguredo_moqt --lib`
  - `cargo test -p shiguredo_moqt --test test_playout`
  - `cargo test -p pbt`
  - `cargo test -p moq-sub`
- 5 秒ごとのログで `gaps` / `gap_ms` / `underruns` / `underrun_ms` / `player_buffer_min` /
  `concealed` / `compressed` / `skew_us` を読む。`underruns` が増えるなら供給が目標遅延を
  超えて止まっている。`gaps` だけが増えるなら隙間の材料化が効いている。
- `audio_only` / `arrival_delay_floor` / `feedback_jitter` / `feedback_applied` /
  `feedback_ceiling` で、音声だけの購読の状態と閉ループの内訳を読む。音声だけの購読では
  目標が 140〜160 ms の範囲へ広がることを確かめる。
- `audio_playout_correction` で、予約した予定に対する実際の出力の遅れを読む。値が大きい
  ままなら、出力のバッファが深いか、予約が遅れている。
- 同じ素材・同じリレーで qmux と QUIC 直接、映像あり / 映像なし、subgroup / datagram を
  比較する。qmux だけで `underruns` が増え、QMux の破棄警告が出ていなければ TCP の
  再送待ちによるバーストが原因である。
- moq-pub の `--target-latency` を 300〜500 ms にして症状が消えるかを見る。消えるなら
  遅延スパイクに対して目標遅延が不足している。
