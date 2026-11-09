# 音声 playout の設計と移植

このドキュメントは、moqt-rs の `playout` モジュールの現在の設計と、moqt-js からの移植の履歴を残すものである。

- 移植元: moqt-js。playout の実装は `f6a171e8` (2026-11-09) の状態を対象にした
- 移植先: moqt-rs
- 履歴は 4 回に分かれる。2026-10-20 の見直し (qmux 経由の音声のプチプチ)、2026-10-22 の追加移植 (moqt-js 0879〜0883)、2026-11-01 の追加移植 (moqt-js `f770493d..6da25d10`)、2026-11-09 の追加移植 (moqt-js `6da25d10..f6a171e8`)

数値は特に断らない限り moqt-js 側の実リレー計測による。moqt-rs の実リレーでの計測はまだ行っていない。moqt-rs の実装は no_std + alloc の Sans I/O であり、時刻は i64 のマイクロ秒で扱う。moqt-js はミリ秒の浮動小数で計算するため、判定に影響し得るのは 1 マイクロ秒未満の差だけである。

## 現在の設計

### 予算の決め方

音声の再生予算 (到着基準で並べるときの再生の遅れ) は、次の 2 つを足して、設定した下限 (既定 50 ms) と上限 100 ms の間に切った値である (`timeline::audio_arrival_delay_us_with_min`)。下限は `TimelineConfig::audio_min_delay_us` で 0〜100 ms から選べる (moqt-js の `audio.minDelayMs`)。

| 入力 | 求め方 | 定数 |
| --- | --- | --- |
| 到着の揺らぎ | 直近 2 秒の到着オフセットの百分位。百分位は配信 fps から決める | `TIMELINE_JITTER_WINDOW_US` |
| 実出力の先行量 | 直近 1 秒の p95 | `TIMELINE_AUDIO_OUTPUT_LEAD_WINDOW_US` / `TIMELINE_P95_PERCENTILE_PPM` |
| 下限 / 上限 | 上記の合計を clamp する | `TimelineConfig::audio_min_delay_us` (既定 `TIMELINE_AUDIO_DELAY_FLOOR_US`) / `TIMELINE_ARRIVAL_DELAY_US` |

- 到着オフセットは (復号の出力の時刻 − TIMESTAMP) であり、基準の遅れ (直近 10 秒の最小値) を引いた値を揺らぎとして数える
- 百分位は `playout_delay_percentile` が `max(0.95, 1 − フレーム間隔 × 1 / 1 秒)` で返す。表示時刻の後に届くフレームを 1 秒あたり 1 枚まで許す値であり、30 fps で約 96.7 %、60 fps で約 98.3 %、120 fps で約 99.2 % になる。音声のフレーム間隔は 20 ms なので約 98 % である
- 実出力の先行量は、いま予約すると鳴り始める位置が到着よりどれだけ先かの p95 である。予算がこの値を下回ると、予約が実行可能な最早位置へ押し上げられ、時間軸が決めた予定と実際の開始がずれる
- catalog の `targetLatency` による下限と、A/V 同期の補償は、この自動予算とは別に扱う
- 追いつき中の出力、停滞後にまとめて復号した出力、長い停止の最初の出力は、揺らぎの分布へ入れない。窓を 2 秒に短くしたことで 1 回の観測で目標が跳ねやすくなったための除外であり、不規則な遅着では追いつきの判定をもう 1 期間だけ確認する
- 古い観測が窓から消えたら必要値へ戻す。映像は、上限を超える観測しか残らない空の分布でも直前の値を保つ
- 時間圧縮の不感帯は `compression_deadband_us` が返す。音声の揺らぎの学習が 2 秒続いていれば `min(既定値, max(5 ms, 揺らぎ))`、まだなら既定値 (20 ms) である

### 時間軸と A/V 同期

- 表示時刻 = TIMESTAMP + 基準の遅れ + 表示の遅れ
- 基準の遅れは、トラックごとの (復号の出力の時刻 − TIMESTAMP) の直近 10 秒の最小値である
- 表示の遅れは、jitter buffer の遅延に `targetLatency` の下限と同期の制御が足した分を加え、上限で切った値である
- 共有する表示時刻の下限は保持しない。2 つのトラックの自然な表示の遅れから、現在の必要量を観測のたびに引き直す。基準を共有できるときは固定の先行量を残さず、先行する側を後行側と同じ表示時刻へ揃える。5 ms の許容幅 (`TIMELINE_SYNC_MIN_DELTA_US`) は、基準を共有できず映像だけを音声の到着基準へ合わせる場合にだけ残す
- 基準の差には 100 ms の保護 (`TIMELINE_MAX_COMPENSATED_DIFFERENCE_US`) を掛け、既知のバッファ差は別に補償する (`sync_compensation_limit_us`)
- 予約した時点の予定と実際に鳴った位置の差 (残差) は、重み付き平均で 250 ms の時定数に平滑化し、端数は四捨五入して映像の合わせ先にだけ足す。切り捨てを続けると定常状態で下振れが積もるため、四捨五入する。有効な観測が 1 秒途切れたら失効させる
- 映像の表示実績は、描画を観測した時刻で記録する。write 時刻ではなく、`present` が戻った後に描画を観測した時刻であり、画面に出た時刻でもない。`present` が戻ってから画面に出るまでの遅れ (コンポジタのフリップ) は測れない
- 映像を選ぶときの目標表示時刻は、呼び出し側が実測した表示周期 (tick 間隔) から推定する。推定は選択の目標にだけ使い、表示の実績は観測のままにする。moqt-js 0885 が実績を推定に変えたのは、ブラウザに表示を観測する手段が無いためである
- 映像の表示の遅れの上限 (`MAX_PRESENTATION_LAG_US`、既定 20 ms) は、映像が遅れて描かれる量の上限であり、A/V のずれ (`skew_us`) にそのまま現れる
- 基準の差が、表示の遅れの上限から決まる閾値を超えるときと、50 ms (`TIMELINE_BASE_DRIFT_US`) を超えて動き続けている (TIMESTAMP が壁時計からずれている) ときは、基準を共有しない。解除は 30 秒保持するが、原因が去って差が戻った状態が 2 秒続いたら保持を待たずに戻す
- 音声が基準を共有できないときは、同期を止めずに映像だけを音声の到着基準の遅れへ合わせる。止めると相対関係を見る相手がいなくなり、A/V がずれたまま残る
- ドリフトした側の TIMESTAMP は使わず、`present_us` が None を返す。呼び出し側はそのトラックを到着基準で並べる

### 欠落と補間

- moqt-rs の出力 (`raw_player`) は FIFO であり、隙間を積まないと後続が早く鳴って出力が枯れる。そのため隙間は書き込みの前に必ず材料化する (`gap::materialize`)。材料化するのは前の音の終わりから今回の開始までの全長であり、下限や上限で切り捨てない
- 隙間は先頭から続き音で埋める。続き音は `expansion::expand_audio` が作る最大 100 ms の音であり、周期が確かな音では `stretch::conceal` の周期補間、それ以外は `predictor::generate_unvoiced` の線形予測へ非周期の励振を入れて合成する。生成した音は最新の実音の RMS とピークを超えないように抑える
- 続き音の材料にする直前の実音の末尾 30 ms は `PlayedAudioHistory` として保持する (example の実装)。moqt-js の `AudioRenderConcealer` と同じく、隙間の材料化の `previous` と時間伸長の `history` へ渡す。履歴に入れるのは実音だけで、隙間の材料化で作った音は入れない。サンプルレートやチャンネル数が変わったときと、媒体時刻が 1 µs を超えて飛んだとき (欠落、配信元の切り替え、時刻の巻き戻り) は作り直す
- 隙間の先頭は、直前の音の最終サンプル (続き音があるときはその先頭の値) から 0 へ向かう重みで繋ぐ。続き音が隙間の後ろ 2 ms まで続くときは、隙間の末尾を 0 へ落とさず、`splice_fade::cross_fade_head` で今回の音の先頭へクロスフェードする。続き音が届かないときだけ 2 ms のフェードで 0 へ落とす
- 時間圧縮は波形のピッチ周期 1 つ分を削る操作である。周期は 4 kHz へ間引いた信号の自己相関から求めて放物線補間で端数を出し、削る位置はクロスフェードで波形を打ち消さない位置を探す (`stretch::compress`)。削る長さは周期で決まり、要求では選べない
- 再同期で到着基準の深さが浅くなったときは、時間伸長で戻す。`stretch::preemptively_expand` は 20 ms 以上の音では直前の実音 10 ms と今回の実音を重ね、20 ms 未満の音では最大 30 ms の履歴で求めた周期を今回の音の先頭で重ねる (NetEq の PreemptiveExpand と同じ重ね合わせであり、上流全体の移植ではない)。scheduler は到着基準で予定が到着 + 再生の遅れより不感帯以上に前になっているときに `expand_us` を要求し、実際に伸ばした長さは確定する実際の長さに含める
- 隙間の区間は、予約できる最も早い時刻より前の分 (補間できない)、補間してよい長さ、上限を超えて無音で残る分に分けて返す。補間の上限は 100 ms、最小は 500 µs である。500 µs 以下の隙間は補間せず、前の音の終わりへ揃えて 1〜2 サンプルの無音を作らない
- 44.1 kHz は解析用の信号だけを 48 kHz の格子へ換算し、求めた周期を元のサンプルレートへ戻すため、時間圧縮も補間も使える。操作できるのは 8 kHz / 16 kHz / 32 kHz / 48 kHz / 44.1 kHz であり (4 kHz の倍数へ丸めて間引きの表を引く)、それ以外のレート (例: 96 kHz) では操作しない。対応するかどうかは `is_time_stretch_available` で確かめられる
- 5 / 10 ms の短いフレームは、現在の音だけでは 4 kHz の相関窓 (61 サンプル、15.25 ms) を満たさない。時間圧縮は直前の実音 (最大 30 ms) と連結した窓で周期を探し、現在の切断位置でもその周期が続いているときだけ使う。時間伸長も、20 ms 未満の音では重ね合わせの位置を履歴の先頭に置き、最大 30 ms の履歴から周期を求める

### 並べ方 (timestamp 経路と到着基準)

- **予約は計画と確定に分ける。** `AudioPlayoutScheduler::schedule` は鳴らす時刻・詰める長さ・伸ばす長さ・隙間だけを返し、予約位置も統計も進めない。呼び出し側が時間圧縮・時間伸長・サンプルへの丸めを適用して出力へ積んだ後に、実際の開始位置と長さで `commit` する。加工や投入に失敗した計画は次の計画で失効し、予約位置を巻き戻す必要が無い。計画は識別子を持ち、古い計画や二重の確定は拒否する
- 時間軸が TIMESTAMP から目標の時刻を決められるときは、その時刻に従って並べる (`AudioPlayoutBasis::Timestamp`)
- 鳴らす時刻は、目標の時刻と「今から 10 ms (`AUDIO_PLAYOUT_MIN_LEAD_US`) 以降かつ直前の音の終わり以降」の大きい方である。500 µs 以下の差は直前の音の終わりへ揃えて、1〜2 サンプルの無音を作らない
- 目標に間に合わない音も捨てない。ずらした分は、遅れと音の長さの半分とキューの余裕の最小値まで時間圧縮で戻す。不感帯未満の遅れでは詰めない。到着基準では、隙間を挟む音 (遅れて届いた音) は詰めない
- 目標から 500 ms (`AUDIO_PLAYOUT_MAX_LATENESS_US`) を超えて離れ、かつ音が途切れているときは、目標を到着基準へ置き直す。音がまだ鳴っている間は並べ直さない (続きが前へずれる)
- 到着基準では、最初の音の基準を「到着した音がまだ鳴っていない位置 + 再生の遅れ」に置き、以後は TIMESTAMP の間隔どおりに並べる。過ぎてから届いた音は今から鳴らせる最も早い時刻へずらし、目標の深さを再度足さない。予定が到着 + 再生の遅れより不感帯以上に前なら、時間伸長して深さを戻す
  - example はこの「到着した音がまだ鳴っていない位置」に、音を受け取った時点の壁時計 (`now_us`) を渡す。moqt-js の `arrivalSeconds` は `AudioContext` の出力位置であり、moqt-rs には出力時計との対応が無いため受信時刻で代用する。媒体の PTS は渡さない
- 並べる音が溜まりすぎているときだけ捨てる (`Drop` の理由は `Backlog`)。timestamp 経路では `delay_us` と `presentation_delay_us` の大きい方に、到着基準では `arrival_delay_us` に、それぞれ 250 ms (`AUDIO_PLAYOUT_BACKLOG_US`) を足した値を上限とする。**上限は時間圧縮の成否に関係なく、未加工の音の終端で検査する。** 圧縮が成立しない波形でも保持量が増えすぎないようにするためである
- moqt-rs の出力は FIFO であり、出力が枯れている間は前の音の終わりを今へ進める必要がある。そのため moqt-js には無い `note_underrun` を持つ

### 音声だけの購読

- 音声だけを購読するときも、予算は設定した下限〜100 ms の自動予算である。映像に合わせる相手がいないため、到着基準で並べる
- 2026-10-22 に追加した下限 140 ms と上限 160 ms、穴 1 個で上限を広げる制御は、2026-11-01 に撤去した。実出力の先行量を予算へ入れたことで、到着基準の待ちに固定の下限を足す必要が無くなったためである。この撤去を直接支える音声だけの A/B は moqt-js の docs に無く、moqt-rs 側での取り直しは未実施である
- 映像を購読していないため、A/V 同期と基準の共有の判定は働かない。時間軸は音声の基準の遅れと予算だけを使う

### 停止と drain

- 再生を止めるときは、予約した音を鳴らし切るまで待つ (drain)。終える条件は「再生機器の再生サンプル数 >= 積んだサンプル数」かつ「今 >= 最後の予約の終端」であり、どちらかが満たされなくても待ち時間の上限に達したら終える
- 待ち時間は「最後の予約の終端 − 今 + 余裕 100 ms」を上限 300 ms で切った値である。上限で切った分は、`record_stopped` が予約したまま鳴らなかった音として計器に残す
- 復号前遅延を移植していないため、flush した Object の復号出力を待つ条件は無い
- `AudioPlayer::stop()` は `record_stopped` の後に呼ぶ。計器が読む値を失わないためである
- 結果は停止時のログ (`Audio playout drained: pending=...ms requested=...ms waited=...ms cut=...ms`) と、5 秒ごとのログの `drain=pending_ms=... requested_ms=... waited_ms=... cut_ms=...` で読める。`pending_ms` は開始時に鳴り終わっていなかった長さ、`requested_ms` は待つと決めた時間、`waited_ms` は実際に待った時間、`cut_ms` は鳴り終わらずに切った長さである

### 計器

- 予算の内訳: `PlayoutTimeline::audio_delay_stats` が `jitter_us` / `output_lead_us` / `target_us` を返す。`compression_deadband_us` は圧縮の不感帯を返す
- 遅延の内訳: `PlayoutTimeline::delay_breakdown` が音声と映像の基準の遅れ・jitter buffer の遅延・同期の補償・表示の遅れ・上限、基準を共有しているかとその理由、基準の差の動き、音声の予算、音声の残差を返す
- 並べ方: `AudioPlayoutScheduler` が実際に詰めた量 (`compressed_us`)、補間した量と回数 (`concealed_us` / `concealments`)、実際に伸ばした量 (`expanded_us`)、捨てた数 (`drops`)、基準を取り直した回数 (`rebases`)、現在の遅れ (`lateness_us`) を持つ。これらは計画ではなく `commit` した実際の開始位置と長さから求める
- 確定の返り値: `commit` は `AudioPlayoutPlay` (`arrival_us` / `target_start_us` / `start_at_us` / `played_us` / `basis`) を返す。呼び出し側はこれをそのまま `AudioPlayoutTimingStats::record_play` へ渡す。`played_us` は時間圧縮・時間伸長・サンプルへの丸めを適用した後の、実際に積んだ長さである
- 映像の選択: `PlayoutNearestSelection` が `draw` / `dropped` / `draw_presentation_us` / `draw_late` を返す。呼び出し側は提示しなかったフレームの数と、逃げ道で遅れて提示した回数を計器とログへ記録する
- 再生の観測: `AudioPlayoutTimingStats` が、予定に対する余裕・到着から鳴り始めるまで・予定からの遅れの分布 (直近 10 秒)、鳴らした音と鳴らさなかった音の累積、理由ごとの内訳、補間で埋まらなかった区間 (回数・合計・最大)、出力が枯れた回数と長さ、時間圧縮も補間も使えないサンプルレートの音の数を返す
- `stretch_unavailable_frames` は時間圧縮・補間・伸長が使えないサンプルレートの音の数である。moqt-js の `sampleRateMismatchFrames` / `contextSampleRate` / `decodedSampleRate` は出力側の PCM キューのレート変換用の計器であり、moqt-rs は `playout::resample` を呼び出し側が使うかどうかを知らないため移植していない
- 同期ずれの鮮度は、実績の新しい方ではなく、呼び出し側が渡す現在時刻 (`skew_us(now_us)`) で測る。両方の出力が止まった後も過去の値を返し続けないためである
- 実際に描画した未補間区間とバッファ境界の段差は、`record_rendered_gap` と `record_rendered_boundary` で記録できる。停止時に鳴らし切るまでの残りは `pending_us` と `pending_end_us` で読める
- 穴の計器は、材料化した隙間の全長から実際に補間した長さを引いた「補間で埋まらなかった長さ」を数える。moqt-js の `gapCount` / `gapMs` と同じ意味である。予約できる最も早い時刻より前だった前置きは `unrecoverable_gaps` として別に数える

## 移植の履歴

### 2026-10-20 の見直し (qmux のプチプチ)

#### 結論

- moqt-js の playout の純粋な計算 (時間圧縮、補間、遅延推定、閉ループ、追いつき、時間軸) は行単位で移植されており、数式・定数・境界に聴感差を生む差異は無かった
- プチプチの主因は移植ミスではなく、再生方針と出力経路にあった。moqt-js 側でも同じ問題 (補間しなかった隙間が境界の段差になる、予約できない隙間がアンダーランになる) が実測で確認されており、moqt-rs は出力が FIFO のため、隙間が無音としても残らず再生が早期化する分だけ悪化し得る
- qmux は原因を増幅する。QMux の DATAGRAM は TCP 1 本の順序付きストリームに載るため、TCP の再送待ちで音声がまとめて遅れて届く。受信バッファの上限を超えると破棄される

#### 一致していた点

この比較は 2026-10-20 時点のものであり、当時は遅延推定 (`delay.rs`) と閉ループ (`feedback.rs`) も移植していた。両者は 2026-11-01 に削除した。

- 時間圧縮・補間: `src/playout/stretch.rs` と moqt-js の `audioTimeStretch.ts` は、4 kHz への間引き、相関窓、ピッチ探索、クロスフェード、放物線補間、継ぎ目の段差判定、末尾のフェードまで一致する
- 再生スケジューラ: `src/playout/scheduler.rs` と `audioPlayout.ts` は、目標時刻の決定、並べすぎの判定、到着基準へのフォールバック、詰める量の式、隙間の条件が一致する
- 時間軸: `timeline.rs` と `timeline.ts` は、基準の 10 秒窓最小値、A/V 同期の不感帯、補償上限、ドリフト判定まで一致する
- 遅延推定・閉ループ・追いつき: `delay.rs` / `feedback.rs` / `catch_up.rs` は、窓・分位点・忘却・上限・増減の速さ・床・cooldown まで一致していた (閉ループは 2026-11-01 に撤去)

#### 対策 (2026-10-20)

- 再生の隙間を必ず材料化する。スケジューラは隙間の全長と補間してよい長さを分けて返し、呼び出し側が隙間を積む。隙間の前後は 2 ms で 0 に落として段差を消す (`src/playout/gap.rs`)。材料化の原則は現在も維持している。隙間の中身は 2026-11-01 に、無音ではなく続き音 (`expansion::expand_audio`) で埋める方式へ変えた
- 復号した音声はその場で積み、最初の目標時刻に達してから再生を開始する。これにより目標遅延ぶんの先行バッファが蓄積される。映像の vsync は無効にする
- 受信した音声の到着時刻と復号完了時刻をフレームへ載せ、時間軸への観測には復号完了時刻を使う。datagram の sleep は空バッチのときだけにし、音声は毎ループ全件取り出す
- 映像 jitter の上限超過サンプルの除外と、基準の共有解除の往復検出を移植する
- 出力が枯れたらその量を目標遅延の閉ループへ入力する、という対策は 2026-11-01 に撤去した。現在は枯れた量を計器 (`AudioPlayoutTimingStats::record_underrun`) で見るだけで、予算には戻さない

### 2026-10-22 の追加移植 (moqt-js 0879〜0883)

#### 0879 目標遅延の内訳

- `PlayoutTimeline::audio_delay_feedback_stats()` を追加した。音声だけを購読するときは A/V 同期の推定を出さないため、閉ループが決めた目標遅延の内訳をここから読めるようにする
- 閉ループの snapshot に、揺らぎだけから求めた目標 (`jitter_target_us`) と実際に適用する値 (`applied_us`) を追加した
- 2026-11-01 に閉ループごと撤去した。現在は `audio_delay_stats` が `jitter_us` / `output_lead_us` / `target_us` を返す

#### 0881 音声だけの到着基準再生の遅れの下限

- `TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_FLOOR_US` (140 ms) を追加した。音声だけを購読すると映像に合わせる相手が無く到着基準で並べるが、当時の閉ループの適用上限が 100 ms のため、Wi-Fi の周期的な停滞 (約 0.5 秒ごとに 100 ms 前後) を吸収しきれなかった
- moqt-js の実リレー Wi-Fi 計測の A/B (0 / 120 / 140 ms を各 6 回、60 秒) で 140 ms を採用した。0 ms は 6 回すべてで無音の穴 (最大 116 ms、復旧できない分 28〜43 ms) と境界の段差が出た。120 ms は穴が 1〜12 ms まで縮んだが 6 回すべてで境界の段差が 1 回出た。140 ms は 4 回で穴も段差も 0、残る 2 回は経路が 140 ms を超えて跳ねた回で 33 ms の穴 (復旧できない分 16〜17 ms) が 1 回だけであった
- 映像を購読しているときはこの下限を渡さない (A/V のずれになる)
- **2026-11-01 に撤去した。** 現在は音声だけでも 50 ms を下限とする自動予算 (50〜100 ms) を使う。この A/B は履歴として残す

#### 0882 穴 1 個での上限拡大と、音声だけの上限

- `AudioDelayFeedback` に穴 1 個で一時的に広げる適用上限 (`gap_ceiling_us`) を追加した。穴は事後には埋められないため、1 個でも「経路の揺らぎが目標を超えた」証拠として扱う
- 穴の長さ + 余白 (20 ms) を吸収できる値まで適用上限を広げ、最後の穴から `AUDIO_DELAY_FEEDBACK_GAP_QUIET_US` (5 秒) の間だけ保持する。保持中も目標を上げる向き (遅れ・並べすぎ・穴) は許すが、下げる向きへは進まない
- 広げる値は `AUDIO_DELAY_FEEDBACK_ARTIFACT_CEILING_US` (160 ms) と明示設定 (`targetLatency`) の小さい方で切る。既存の artifact ceiling (圧縮か穴が 2 秒続いたときに 160 ms へ広げる規則) は残し、穴 1 個の一時的な広げと段階を分ける
- `TIMELINE_AUDIO_ONLY_ARRIVAL_DELAY_MAX_US` (160 ms) と `audio_only_arrival_delay_us(learned_delay_us, floor_us)` を追加した。音声だけの購読では下限 140 ms を確保したうえで、閉ループが穴で広げた目標を 160 ms まで使う。共有の基準があるトラックの上限 (`TIMELINE_ARRIVAL_DELAY_US` の 100 ms) は変えない
- `AudioDelayFeedbackObservation` に、累積の穴の数と長さ (`gap_count` / `gap_us`) と、音声だけの到着基準の下限 (`arrival_delay_floor_us`) を追加した
- **2026-11-01 にすべて撤去した。** 現在は揺らぎと実出力の先行量の clamp だけで予算を決め、穴を根拠に上限を広げない

#### 0883 A/V 同期の再設計

- 同期が足す前の表示時刻から、共有する表示時刻の下限を 1 つだけ決めるようにした。必要値が増えたら即座に上げ、減ったら毎秒 `TIMELINE_DELAY_DECAY_US_PER_SECOND` (20 ms) までで戻す。過去に各トラックへ足した補償を必要値の算定へ戻さないため、揺らぎが上下しても待ちが積み上がらない
- 各トラックの補償は共有の下限から毎回計算し直す。自分の jitter buffer が補償を埋めた分は差し引く (表示時刻を前へ動かさずに余分な補償を解消できる)
- 基準の差には `TIMELINE_MAX_COMPENSATED_DIFFERENCE_US` (100 ms) の保護を掛け、同じ向きの既知のバッファ差は別に補償する (`sync_compensation_limit_us`)。先行側は両方を合成した実際の表示時刻から決める
- 予約した時点の予定と実際に出力された時刻の差 (残差) を 250 ms の時定数で平滑化し、映像の合わせ先へ足す (`record_presentation_with_target`)。音声の予測 (予定どうし) だけを揃えると、実際に鳴る音から映像が先行したままになる。残差は `DelayBreakdown::audio_playout_correction_us` で読める。有効な出力の観測が 1 秒途切れたら残差を失効させる
- 閉ループの開始値を安全下限 (50 ms) にした。最初の再生の観測だけで 100 ms を適用すると、静かな経路でも映像が跳ねて遅れる。経路と出力に必要な分は観測から増やす
- `AudioDelayFeedback::reset()` を追加し、音声の再生を取り直したら過去の穴と遅れを持ち込まないようにした。`PlayoutTimeline::reset_track` / `reset` から呼ぶ
- 目標を広げた呼び出しの中で同期も更新する。次の復号出力まで待つと、穴からの復帰時にその間だけ音声と映像の時刻が離れる
- 音声の観測が無く再生の観測だけがある場合も、閉ループの目標を到着基準の遅れに使う (`playout_delay_us` のフォールバック)
- `targetLatency` を設定した時点で、実際に使う音声の遅れも更新する。次の観測を待つと、停止中などでは計器だけが新しい上限になり、実際の保持値には古い目標が残る
- 2026-11-01 に「共有する表示時刻の下限を保持する」方式をやめ、現在の必要量を観測のたびに引き直す方式へ変えた。残差の 250 ms 平滑化と失効、毎回の引き直しは現在も維持している。閉ループの開始値と `AudioDelayFeedback::reset` は閉ループごと撤去した

#### 周期探索の修正 (audioTimeStretch.ts)

- 補間の末尾周期の探索で、細探索の範囲を 1 サンプル外側まで広げ、放物線補間に必要な隣接点を実際に評価するようにした。探索範囲の端に最大があるときに、評価していないラグで外挿して周期が 0.5 サンプルずれる問題を直す
- 整数ラグが丸め誤差の範囲で完全相関のときは、放物線の頂点へずらさない。有限窓の前後の相関は非対称になり得るため、正しい整数周期を頂点へずらすと位相を壊す
- この修正は現在の `src/playout/stretch.rs` に残っている。2026-11-01 には、圧縮の削る位置をクロスフェードが波形を打ち消さない位置から選ぶ処理と、44.1 kHz の解析対応を加えた

### 2026-11-01 の追加移植 (moqt-js `f770493d..6da25d10`)

moqt-js が 2026-10-22 以降に変えた設計判断を moqt-rs へ移した。対象は次の 6 コミットの内容である。

- `f0f931d9`: 復号フレームのレートに解析を揃える。圧縮側の周期推定に端数補間を入れる
- `c3f36fa4`: 揺らぎ窓の分離、出力の先行量の導入、`AudioDelayEstimator` と `AudioDelayFeedback` の削除、同期の作り直し
- `e75d866f`: 起動時の停滞を通常の再生遅延として学習しない (再確認を 1 回だけ許す)
- `8927c50c`: 出力側補間へ移す前提の整備 (境界の接続、収録、計測)
- `99a8c10c`: 欠落時だけ出力側で補間する方式へ移行し、旧経路を削除する
- `20985ca8`: 再同期後の深さを時間伸長で回復する。準備、出力切替、復号器の復旧、drain を修正する

#### 削除した制御

- `src/playout/delay.rs` (`AudioDelayManager`) と `src/playout/feedback.rs` (`AudioDelayFeedback`) を削除した。対応するテストと PBT (合計 3,579 行) も削除した
- 削除の理由は 4 つある。制御が 4 つ重なっていた (区間最大のヒストグラム、穴と圧縮による適用上限の拡大、共有の表示時刻の保持、映像の緩やかな減衰)、実出力の先行量を直接測れるようになり「上限を育てる」代理指標が不要になった、閉ループは遅延を増やす方向にしか効かない、独自方式が NetEq より優れているという測定結果が無い
- 削除しても残したものは、時計のドリフト検出、基準の共有解除と復帰、出力時計の fallback、catalog の明示下限、PLC、波形の時間圧縮、出力の残差の 250 ms 平滑化である
- 削除前の実害は数値で残っている。到着基準の予約で出力の先行量を無視していたため、目標 100 ms に対して鳴り始めが 195.5 ms になっていた。回復後に約 188 ms の音声遅延を保持し続けていた

#### 実出力の先行量

- 先行量の定義は「いま予約すると鳴り始める位置が、到着した時刻よりどれだけ先か」である。moqt-js は `getOutputTimestamp()` の `contextTime` と `performanceTime` の対応から到着時刻を `AudioContext` の時計へ換算し、`currentTime + 10 ms − 換算した到着時刻` を取る。10 ms は描画量子 (48 kHz で約 2.7 ms) と `start()` の遅れを見込んだ下限である
- 直近 1 秒の p95 を採り、揺らぎに足して 50〜100 ms に切る
- 解決したのは「予算が実行不可能になる」問題である。予算が先行量を下回ると予約は最早位置へ押し上げられ、時間軸が決めた予定と実際の開始がずれる。そのずれは詰めと穴の判断を狂わせる。実測では、揺らぎだけを使う案で揺らぎ条件の穴が約 168 ms 残り、比較元は約 35 ms だった。先行量を足した案では同じ条件の穴が 3.8〜4.4 ms になった
- 二重に足さないための規則も決まっている。予約は `currentTime` の軸で行い、`getOutputTimestamp()` と `performance.now()` の対応で到着を同じ軸へ移す。出力時計が未確立の間は `currentTime` で代用し、代用値を実出力の観測として学習しない
- moqt-rs では、`raw_player` の `total_samples_enqueued` と `total_samples_played` の差から、スケジューラが既に並べた分を引いた値を先行量として `PlayoutTimeline::observe_audio_output_lead` へ渡す
- 出力レートへの変換は `playout::resample` に移植した。同じレートとアップサンプリングは線形補間、ダウンサンプリングは Blackman 窓付き sinc で帯域を制限する。フレームをまたぐ連続 PCM のキュー (`AudioRenderQueue`) は出力時計を持つ適用層が前提であり、moqt-rs にはその層が無いため移植していない

#### 揺らぎの窓と百分位

- 基準の 10 秒窓 (`TIMELINE_WINDOW_US`) と、揺らぎの 2 秒窓 (`TIMELINE_JITTER_WINDOW_US`) を分けた。10 秒窓では数秒の停滞がその後の 10 秒間ずっと目標を押し上げ、長い窓だけで増やすと行き過ぎるためである
- 2 秒にしたことで、10 秒窓を前提にしていた映像の減衰 (毎秒 20 ms) が不要になり、百分位の値をそのまま使えるようにした。映像の表示の遅れは減衰しない
- 百分位を映像だけでなく音声にも適用するようにした。音声のフレーム間隔は 20 ms なので分位は約 0.98 になり、固定 0.95 より深い予算になる
- 1 回の観測で目標が跳ねないよう、追いつき中の出力、まとめて復号した出力、長い停止の最初の出力を分布へ入れない。不規則な遅着では追いつきの判定を 1 期間だけ再確認する
- 時間圧縮の不感帯を、最初の 2 秒は 20 ms、その後は 5〜20 ms にした。揺らぎの学習が 2 秒続いてから細かく詰める

#### 同期の作り直し

- 下限を保持せず、観測のたびに現在の必要量を計算し直す方式に変えた。合わせる量は「基準の遅れ + 表示の遅れ」の差そのものであり、直近の観測の相対遅延ではない
- 不感帯は 5 ms、基準の差の補償は最大 100 ms、既知のバッファ差と出力の残差は別に補償する
- 音声の TIMESTAMP が信用できず基準を共有できないときは、同期を止めずに映像だけを音声の到着基準へ合わせる
- 効果は「停滞が去った後に待ちを残さない」ことである。実測では、音声だけの 180 ms 停滞で回復後の音声遅延が 187.7 ms から 70.1 ms になった。100 ms を超えた最後の観測から 100 ms 未満へ戻るまでの時間は 0.68〜3.22 秒で、比較元は 10 秒の観測終了まで戻らなかった

#### 出力側補間と時間伸長

- moqt-js は、実音は `AudioBufferSourceNode` で予約し、`AudioWorklet` は正常区間の PCM をそのまま通し、実際の未着時だけ補間する方式へ移った。補間は最大 100 ms、末尾 30 ms の履歴から、有声は周期補間、非周期は線形予測で作る。実音へ戻るときと停止時だけ 2 ms で接続する
- moqt-rs は `AudioWorklet` に相当する適用層を持たない。純粋な DSP だけを移植し、`src/playout/stretch.rs` (周期補間、時間圧縮、時間伸長)、`src/playout/expansion.rs` (最大 100 ms の続き音)、`src/playout/predictor.rs` (12 次の線形予測と非周期励振の数値カーネル)、`src/playout/splice_fade.rs` (接続フェード、隙間の直線補間、`f32-planar` の統合) に置いた
- 時間伸長 (moqt-js の `preemptivelyExpandSamples` 相当) を追加した。再同期で浅くなった到着基準の深さを、音を止めずに周期 1 つずつ戻す。scheduler が `expand_us` を要求し、適用結果は確定した実際の長さで基準と直前の音の終わりへ反映する
- 非周期音の 12 次の積和を小さな Wasm カーネルへ移す措置は、ブラウザの実行時間を守るためのものである。moqt-rs は同じ演算を `predictor.rs` の Rust で行い、Wasm は同梱しない
- 44.1 kHz の解析対応を入れた。解析用の信号だけを 48 kHz の格子へ換算し、求めた周期を元のサンプルレートへ戻す。元の音声はリサンプルしない
- **適用層の違いを既知の差異として扱う。** moqt-js は出力側で実欠落だけを補間する。moqt-rs は `raw_player` の FIFO へ PCM を書き込むため、隙間は書き込みの前に材料化する。DSP は共通だが、呼ばれる位置が違う

#### 音声だけの下限の撤去

- 音声だけの購読に置いていた下限 140 ms と上限 160 ms を撤去した。音声だけでも到着基準の予算を 50 ms に戻した
- 根拠は、出力の先行量を予算へ入れたことで、到着基準の待ちに固定の下限を足す必要が無くなったことである。採用した変更の一覧にも「音声だけの到着基準の再生は、前段階で検証した 50 ms の予算を保つ」と書かれている
- 注意すべき点がある。140 ms 下限を外した判断を直接支える A/B は moqt-js の docs に見当たらない。旧 140 ms の根拠は Wi-Fi の周期的な停滞を対象にした A/B であるため、moqt-rs 側で同じ条件を取り直すか判断を保留するかは未決である

#### 計器の変更

- 出力側で実際に描画した未補間区間とバッファ境界の段差を記録する `record_rendered_gap` と `record_rendered_boundary` を追加した。moqt-rs では出力側の描画量を測る層が無いため、呼び出し側が実際に書き込んだ PCM を測ったときだけ使う
- 停止時に鳴らし切るまでの残りを読む `pending_us` / `pending_end_us` を追加した。`record_stopped` で停止時に切った予約を数える
- 時間圧縮も補間も使えないサンプルレートの音を数える `stretch_unavailable_frames` を持つ。moqt-js の `stretchUnavailableFrames` に対応する。44.1 kHz は解析対応したため使い、8 kHz / 16 kHz / 32 kHz / 48 kHz / 44.1 kHz 以外のレート (例: 96 kHz) を数える
- moqt-js の `sampleRateMismatchFrames` / `contextSampleRate` / `decodedSampleRate` は `AudioBufferSourceNode` のリサンプル専用の計器であり、moqt-rs に該当する段が無いため移植していない (`stretch_unavailable_frames` とは別の系統である)
- 閉ループの計器 (`feedback_*`、`arrival_delay_floor`) は削除した。代わりに `audio_delay_stats` の `jitter_us` / `output_lead_us` / `target_us` と、`compression_deadband_us`、`delay_breakdown` を読む

### 2026-11-02 の追加移植 (moqt-js 0885)

moqt-js が映像の表示をディスプレイのリフレッシュ (vsync) に整列し、表示先の提示遅延を補償する変更を入れた (issue `0885`)。moqt-rs へ移したのは選択規則と計器の意味であり、ブラウザ固有の層は移していない。

#### 変更内容

- 選択 API を `select(nowMs)` から `selectNearest(targetMs, toleranceMs, maxLateMs)` へ変えた。目標表示時刻に最も近い 1 枚を選び、目標から `maxLateMs` より古いフレームは提示しない
- 戻り値を `draw` / `dropped` / `drawPresentationMs` / `drawLate` に変えた
- `MAX_PRESENTATION_LAG_MS` (20 ms) の意味を「表示時刻を過ぎたフレームを 20 ms まで遅れて描く」から「表示できるフレームが無いときだけ遅れて提示する逃げ道の上限」へ変えた
- 表示先の提示遅延 (`presentationLatencyTicks`、0〜4 tick) を必須の入力にした。canvas は 0、generator は 1 である
- 表示周期を rAF の間隔の下位 25 % から推定する (`estimateRefreshMs`)
- 表示実績を「`present` の呼び出し時刻」から「その tick の目標表示時刻」へ変えた
- `VideoPlayoutPacing` (`drainImmediately` / `framesPerDrain`) を削除し、rAF の tick ごとに 1 枚へ統一した

#### moqt-rs に移植したもの

- `select_nearest` の選択規則。目標から `max_late_us` より古いフレームを捨て、`target - max_late` から `target + tolerance` の範囲で目標に最も近い 1 枚を選ぶ。同距離なら先に積んだ方である。選んだフレームより前のフレームは捨てる。`draw_late` は選んだフレームの表示時刻が `target - tolerance` より古い場合に true になる
- `dropped` / `draw_late` の意味。`dropped` は提示しなかったフレーム (遅れ、同じ表示周期に重なった分) であり、`draw_late` は逃げ道を使った回数として読む。キュー上限超過の破棄は `enqueue` の戻り値と再生機器の統計で別に数える
- `MAX_PRESENTATION_LAG_US` の意味の変更。従来の doc は「60 Hz の表示周期」「30 fps では…」を根拠にしていたが、moqt-rs に vsync は無く、量子化は呼び出し側のループ周期 (1 ms ループで、実測は 1〜3 ms と推定) である
- 表示周期の推定手法 (下位 25 %)。example は tick 間隔を実測し、目標 = tick + 1 tick、許容幅 = 表示周期 / 2、遅れの上限 = max(20 ms, 表示周期) + 1 tick とする
- A/V 同期の扱いは変えない。表示の実績は描画を観測した時刻のまま記録し、推定は選択の目標にだけ使う
- skew 予算の考え方。moqt-js は記録値が推定になったため「write の遅れ」の項を予算から外したが、moqt-rs は記録値が観測のままなので、表示の遅れは実装が許す遅れとして予算に残す (`MAX_PRESENTATION_LAG_US` の逃げ道を含む)

#### 移植しなかったもの

- `presentationLatencyTicks`。moqt-rs には vsync が無く、提示遅延は 1 ループ周期でディスプレイの周期に量子化されない。tick 数を必須の公開 API にすると「表示周期の倍数で遅れる」という誤った模型を押し付ける。example は「1 tick」と近似する
- `estimateRefreshMs` と rAF の予約、`IDLE_STOP_MS` に相当する処理。moqt-rs のループは常時回っており、予約の停止で周期の推定が壊れる問題が無い。推定の手法 (下位 25 %) だけを example に移した
- `VideoPlayoutPacing` の削除。moqt-rs に pacing は無く、削除するものがない
- 実ブラウザ試験 (`requestVideoFrameCallback` による vsync 観測)。raw_player は SDL で、相当する観測が無い
- devtools の遅延・停止レポート。ブラウザの計器であり、対応する層が無い。ただし「投入から描画までの遅れ」「表示時刻の残差」「表示間隔の分布」を example のログに出す考え方は取り込んだ

#### 注意: issue と実装の食い違い

issue の記述と実装が食い違う箇所があり、移植は実装に合わせた。

- 表示周期の推定は、issue の「設計方針」では「直近 16 tick の中央値」だが、実装は下位 25 % である
- 遅れてよい上限は、issue の「提示遅延の分だけ広げる」ではなく、実装は `max(20 ms, 表示周期)` も取る (表示周期が 20 ms より長いときに 1 tick 遅れが 20 ms を超えるため)
- `drawLate` は「`target - tolerance` より古い」の定義であり、`tolerance = 0` を渡すと描いたフレームは常に true になる。moqt-rs の example は `tolerance = 表示周期 / 2` を渡す
- 記録する表示時刻は、moqt-js では「その tick の目標表示時刻」である。moqt-rs は描画を観測できるため、記録は観測のままにする

### 2026-11-09 の追加移植 (moqt-js `6da25d10..f6a171e8`)

moqt-js が音声の予約と出力時計を再設計し、高レベル再生を MediaStream へ統一した。このうち
moqt-rs へ移したのは、予約の計画と確定、予約の上限、短いフレームの周期解析、帯域制限の
レート変換、A/V 同期の固定先行量の撤去、同期ずれの鮮度である。ブラウザ固有の出力層は
従来どおり移植しない。

#### 予約の計画と確定

- `AudioPlayoutScheduler::schedule` は計画 (`AudioPlayoutPlan`) を返すだけにした。予約位置・
  時間伸縮・再同期の実績は、呼び出し側が PCM を加工して出力へ積んだ後に `commit` した
  実際の開始位置と長さで初めて進む。加工や投入に失敗した計画は次の計画で失効する
- 計画は識別子を持つ。古い計画、別の音の計画、二重の確定では予約位置を変えない
- 旧 `confirm_stretch` / `confirm_expansion` / `confirm_concealment` / `confirm_playout_end`
  を削除した。詰め・伸長・実終端は `commit` の前後差から求める
- 時刻基準と到着基準で、予約の上限・前の音との接続・圧縮と伸長の選択を同じ規則にした。
  上限は時間圧縮の成否に関係なく、未加工の音の終端で検査する
- 到着基準では、予定が到着 + 再生の遅れより不感帯以上に前なら時間伸長する。再同期の
  直後だけという限定を外し、浅い状態が続く間は戻す
- 出力が枯れている間の `note_underrun` は moqt-rs の出力が FIFO であるために残した

#### 時間軸と A/V 同期

- `TimelineConfig::audio_min_delay_us` を追加した。到着基準の再生の遅れの下限を 0〜100 ms
  から選べる (既定 50 ms)。moqt-js の `audio.minDelayMs` に対応する
- 基準を共有できるときは、先行する側を後行側と同じ表示時刻へ揃える。固定の 5 ms 先行を
  残さない。`TIMELINE_SYNC_MIN_DELTA_US` は、基準を共有できず映像だけを音声の到着基準へ
  合わせる場合の許容幅として残した
- `skew_us` は現在時刻を受け取る。実績の新しい方ではなく現在時刻で鮮度を測るため、
  両方の出力が止まった後に過去の値を返し続けない
- 圧縮の不感帯は、時刻基準と到着基準の両方で `AudioPlayoutInput::stretch_min_us` を使う。
  低遅延の指定では、呼び出し側が下限の半分 (5〜20 ms) に抑えた値を渡す

#### 短いフレームとレート変換

- 5 / 10 ms の音でも周期を求められるよう、時間圧縮は直前の実音 (最大 30 ms) と連結した
  窓で周期を探し、現在の切断位置でも周期が続いているときだけ使う。時間伸長も 20 ms 未満
  では履歴の先頭で重ねる
- ダウンサンプリング用の Blackman 窓付き sinc を `playout::resample` に移植した。同じ
  レートとアップサンプリングは線形補間、ダウンサンプリングは帯域制限する。フレームを
  またぐ連続 PCM のキューは、出力時計を持つ適用層が前提のため移植していない
- moqt-js の Wasm カーネルの削除は、moqt-rs では該当が無い (線形予測は最初から Rust)

#### 移植しなかったもの

- `AudioClockBridge` (`getOutputTimestamp()` と `performance.now()` の対応、100 ms の
  平滑化、停止の検出)。moqt-rs は `raw_player` の累積サンプル数から先行量を測る
- `AudioWorklet` の出力コールバック、`AudioRenderQueue`、チャンネル配置の変換、
  `AudioRenderConcealer` の出力側の欠落判定
- `mediaPlayout.ts` の MediaStream 統合と Safari 向けの `captureStream(0)`、`video.output`。
  `minDelayMs` は moqt-rs ではライブラリの `TimelineConfig::audio_min_delay_us` と、
  example の `--audio-min-delay-ms` (0〜100 ms、既定 50 ms) で選ぶ

## 移植の範囲

### moqt-rs に移植したもの

- `src/playout/timeline.rs`: 基準の学習、音声の自動予算 (揺らぎの百分位 + 出力の先行量の clamp)、映像の表示の遅れ、A/V 同期 (毎回の引き直し、補償上限、出力残差、共有の解除と復帰)、基準を共有しない判定と理由、遅延の内訳
- `src/playout/scheduler.rs`: 鳴らす時刻の決定 (timestamp 経路と到着基準)、時間圧縮の要求、再同期後の時間伸長の要求、隙間の 3 分割、終端の確定、統計
- `src/playout/stretch.rs`: 時間圧縮 (端数周期、削る位置の選択、短いフレームの履歴解析)、時間伸長 (`preemptively_expand`)、周期補間 (`conceal`)、44.1 kHz の解析対応
- `src/playout/expansion.rs`: 出力側で生成する最大 100 ms の続き音 (周期補間と非周期合成の使い分け、音量の制約)
- `src/playout/predictor.rs`: 12 次の線形予測と非周期励振の数値カーネル
- `src/playout/gap.rs`: FIFO 出力のための隙間の材料化
- `src/playout/splice_fade.rs`: 端のフェード、隙間の直線補間、続き音から実音へのクロスフェード、`f32-planar` の統合
- `src/playout/timing.rs`: 再生の観測値 (分布、累積、穴、境界の段差、枯れ、時間圧縮と補間を使えないサンプルレートの音) と、停止時の残りの計器
- `src/playout/resample.rs`: ダウンサンプリングの Blackman 窓付き sinc と、1 ブロックのレート変換
- `src/playout/catch_up.rs` / `src/playout/buffer.rs` / `src/playout/publish_catch_up.rs`: 追いつき (`AudioReceiveCatchUpObservation` は `now_us` / `lag_us` / `input_lag_us` / `duration_us` / `timestamp_us` をマイクロ秒で受ける)、表示待ちのキューと目標表示時刻に最も近いフレームの選択 (`select_nearest`)、配信側の追いつき
- `crates/moq-sub`: 直前の実音 30 ms の履歴、実出力の先行量と到着基準の深さの観測、時間伸長と接続フェード、終端の確定と計器へ渡す順序、停止時の drain、映像の表示周期の実測と選択の目標、描画を観測した実績の記録、計器の出力

### 移植対象外 (ブラウザ固有)

- Web Audio (`AudioContext`、`AudioBufferSourceNode`、`AudioClockBridge`) と、`getOutputTimestamp()` と `performance.now()` の対応
- `AudioWorklet` (`AudioWorkletProcessor`、`registerProcessor`、`currentFrame`、128 フレームの量子) と、出力コールバックの適用層 (`AudioRenderConcealer`)。DSP だけを移植した
- WebCodecs (`AudioData` / `AudioDecoder`)、`HTMLAudioElement`、`MediaStream`、devtools
- 映像の vsync と rAF の予約、`presentationLatencyTicks` (提示遅延の tick 数)、`estimateRefreshMs`、`requestVideoFrameCallback` による実ブラウザ試験、devtools の遅延・停止レポート
- Wasm カーネル。Rust で同じ演算を書けば同じ結果になり、同梱と CSP の制約も無い
- CSP、自動再生ポリシー、タブのバックグラウンド抑制、`AudioWorklet` の隣接する `process` に同じ `currentFrame` が渡る問題

### 移植しないと決めたもの

- `MediaPlayout` への入口の一本化。これはブラウザ向け公開 API の整理であり、moqt-rs には相当する入口クラスを追加していない。example が timeline / scheduler / timing / DSP を組み立てる
- `PreDecodeDelayQueue` / `PreDecodeDelayStage` / `decodeInputTimestamps`。復号の手前で待つ制御と、復号入力の時刻の対応表を持たない
- `timedValues` / `videoPlayoutSession`。moqt-rs には相当する高レベル API の層が無い

## 既知の差異と限界

- **予約の進行が実投入に従う。** moqt-js も計画と確定を分けるが、計画の識別子はオブジェクト同一性で判定する。moqt-rs は計画ごとの識別子を持ち、古い計画や二重の確定を明示的に拒否する
- **適用層が違う。** moqt-js は出力側 (Worklet) で実欠落だけを補間する。moqt-rs は `raw_player` の FIFO へ PCM を書き込む前に、隙間の全長を先頭から材料化する。隙間の先頭から続き音で埋める点は同じだが、呼ばれる位置と長さの決め方が違う
  - 予約できる最も早い時刻より前の区間 (`unrecoverable_gap_us`) は、moqt-js では補間をその後ろに置くために効く。moqt-rs は FIFO に積むため材料化の総長を変えず、計器の内訳にだけ効く。総長は `total_gap_us` で決まり、音の配置だけが moqt-js と違う
- **実出力の先行量の意味が完全には一致しない。** moqt-js は `AudioContext` の `getOutputTimestamp()` と `performance.now()` の対応から測る。moqt-rs は `raw_player` の累積の投入サンプル数と再生サンプル数の差 (`audio_buffer_ms` 相当) から、スケジューラが既に並べた分を引いて測る。実リレーでの係数調整は未実施である
  - `raw_player` の `total_samples_played` は「デバイスへ書き込んだが SDL のキューに残っていないサンプル数」であり、スピーカーから出た時刻ではない。SDL と OS のバッファの分だけ実際より浅い可能性があり、この差の大きさは未確認である
- **到着基準の実出力の深さは近似である。** FIFO の音声 chunk が切り替わった時点の壁時計から、予約した時点の到着記録を引いて測る。moqt-js の `arrivalSeconds` (`AudioContext` の出力位置) とは測り方が違い、chunk の境界より細かい位置は分からない
- **出力残差の意味が moqt-js と違う。** moqt-js は `AudioContext` の予約時刻との差を使い、下流の要素やデバイスが足す遅延は含まない。moqt-rs は `raw_player` の実際の再生位置から求めるため、プレイヤーの内部バッファを含む。実際に聞こえる音に映像を合わせる目的では moqt-rs の値が正しいが、値の大きさは moqt-js と単純には比較できない
- **穴の計器の意味は moqt-js に揃えた。** moqt-rs の `gaps` / `gap_total_us` は、材料化した隙間の全長 (`total_gap_us`) から実際に補間した長さを引いた「補間で埋まらなかった分」である。材料化した全長は補間の材料を作るための長さであり、穴としては数えない
- **直前の実音の履歴は、欠落の直後だけ 30 ms 未満になり得る。** 媒体時刻が 1 µs を超えて飛んだら履歴を作り直すため、欠落や配信元の切り替えの直後は履歴が短くなる。moqt-js のリングバッファも実音だけを保つが、moqt-rs は媒体時刻の連続性で作り直す判定を明示的に行う
- **TIMESCALE がある TIMESTAMP はメディア時刻として扱う。** LOC の TIMESTAMP は Timescale が無ければ Unix epoch からのマイクロ秒 (壁時計)、あれば (0 を含む) publisher のメディア時刻である。メディア時刻の音は壁時計と対応しないため、時間軸の基準の学習と到着基準の深さの観測には使わない
- **基準の共有解除の保持の満了は、基準の差を記録した時刻から測る。** 片方のトラックが届かなくなると記録が止まり、満了の判断もその時点で止まる (moqt-js と同じ)。記録が再開すると、その間に経過した時間で判定する
- **44.1 kHz は DSP が解析対応したが、実機の確認はしていない。** 操作できるのは 8 kHz / 16 kHz / 32 kHz / 48 kHz / 44.1 kHz であり、それ以外のレート (例: 96 kHz) の音は時間圧縮・補間・伸長が働かず、`stretch_unavailable_frames` に数える。44.1 kHz の AAC で時間圧縮・補間・伸長が動くことと、聴感・圧縮量は未確認である
- **vsync が無いため、提示遅延が tick に量子化されない。** moqt-rs は `set_vsync(0)` で vsync を無効にしており、プログラムから見た表示周期は呼び出し側のループ周期 (example は 1 ms) である。moqt-js の `presentationLatencyTicks` に相当する値は持たない
- **投入から描画を観測するまでの遅れは、まだ実測していない。** 1 ループ周期 (1〜3 ms と推定) の見込みである。example のログに分布を出す
- **`present` が戻ってから画面に出るまでの遅れは測れない。** SDL とコンポジタの実装依存であり、コードからは決まらない
- **映像が遅れて描かれる量は A/V のずれに現れる。** 表示の実績は「描画を観測した時刻」であり、`MAX_PRESENTATION_LAG_US` の逃げ道まで遅れて描くと `skew_us` にもその分が現れる
- **短いフレームの周期解析は、履歴の連続性に依存する。** 5 / 10 ms のフレームで履歴が無い、または媒体時刻が飛んで履歴を作り直した直後は、時間圧縮も時間伸長も働かない。moqt-js と同じ制約である
- **レート変換は 1 ブロック単位で、フレームをまたぐ位相の連続を保証しない。** 呼び出し側が前後のブロックを含めた連続した PCM を渡すこと。moqt-js の `AudioRenderQueue` に相当するキューは持たない
- **実リレー・実機での映像の計測はまだ行っていない。** 表示間隔と `skew_us` の実分布は未確認である
- **実リレーでの計測はまだ行っていない。** このドキュメントの数値は moqt-js 側の計測に基づく

## 検証方法

- 単体・統合・PBT:
  - `cargo test -p shiguredo_moqt --lib`
  - `cargo test -p shiguredo_moqt --test test_playout`
  - `cargo test -p pbt`
  - `cargo test -p moq-sub`
- フォーマットと lint:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt`
- README を書き換えたら `cargo test -p shiguredo_moqt` を通す。README のコード例が doctest としてコンパイルされる
- 削除したシンボルが残っていないことを確かめる。

  ```bash
  rg -n "AudioDelayFeedback|AudioDelayManager|audio_only_arrival_delay|feedback_target|feedback_ceiling|arrival_delay_floor" src tests pbt crates docs README.md skills
  rg -n "confirm_stretch|confirm_expansion|confirm_concealment|confirm_playout_end|last_play\(|gap_start_us" src tests pbt crates README.md skills
  ```

  2 つ目は、moqt-js の計画と確定へ移した旧 API が残っていないことの確認である
  (移植の履歴の記述は除く)。

- 5 秒ごとの `Audio jitter buffer:` 行 (example) で次を読む。
  - `player_buffer` / `player_buffer_min`: 予算が足りているか。0 に張り付かない
  - `dropped` / `concealed` / `concealments` / `compressed` / `expanded`: 捨てた数、補間した量と回数、詰めた量、伸長した量
  - `jitter_ms` / `output_lead_ms` / `target_ms`: 予算の内訳。目標は設定した下限〜100 ms に収まり、音声だけでも 140 ms へ広がらない
  - `deadband_ms`: 圧縮の不感帯。開始の 2 秒は 20 ms、その後は 5〜20 ms (低遅延の指定では最初から下限の半分まで)
  - `gaps` / `gap_ms` / `gap_max_ms`: 補間で埋まらなかった穴
  - `unrecoverable_gaps` / `unrecoverable_gap_ms`: 予約できなかった前置き
  - `underruns` / `underrun_ms` / `underrun_max_ms`: 出力が枯れた回数と長さ
  - `skew_us`: A/V のずれが発散しない
  - `slack_p50_p95` / `start_delay_p50_p95` / `lateness_p50_p95`: 予算を入れた後に悪化しない
  - `missed_backlog` / `missed_stopped` ほかの理由別の捨て: 増えない
  - `stretch_unavailable`: 時間圧縮も補間も使えないサンプルレートの音の数。8 kHz / 16 kHz / 32 kHz / 48 kHz / 44.1 kHz 以外 (例: 96 kHz) で増える
  - `audio_playout_correction`: 予約した予定に対する実際の出力の遅れ。先行量を予算へ入れた後に小さくなる
  - `played` / `played_ms`: `commit` で確定した、実際に鳴る長さの合計
  - `arrival_depth_ms`: 到着基準で予約した音の実出力の深さ (chunk の切り替わりから測る近似)
  - `drain=pending_ms=... requested_ms=... waited_ms=... cut_ms=...`: 停止時に予約を鳴らし切れたかと、切った長さ
- 1 秒ごとの `Video player:` 行 (example) で次を読む。値は判定には使わない。
  - `not_shown`: 提示しなかったフレームの数。遅れ、同じ表示周期に重なった分を含む
  - `draw_late`: 目標に合うフレームが無く、遅れて提示した回数 (逃げ道が働いた回数)
  - `enqueue_to_render=(p50=... p95=... max=...)`: 再生機器へ投入してから描画を観測するまでの遅れ
  - `presentation_residual=(p50=... p95=... max=...)`: 記録した表示時刻 − `present_us` が決めた表示時刻
  - `rendered` / `dropped` / `queue_full` / `late` / `resync` / `drain_recoveries` / `player_resyncs`: 再生機器が観測した描画・破棄・復帰の回数
- `timeline.delay_breakdown` の `sharing_bases` / `unshared_reason` を、映像ありの購読で読む。共有が解除されたまま戻らないなら、基準の差かドリフトの判定を見る
- A/V 同期の制御 (30 秒保持の満了と解除、skew の合成)、出力残差の平滑化 (四捨五入) は `tests/test_playout/timeline.rs` と `pbt/tests/prop_playout/timeline.rs` で固定している。直前の実音の 30 ms 履歴、時間伸長の適用、停止時の drain は `cargo test -p moq-sub` で固定している
- 映像の選択規則 (許容幅の境界、逃げ道で遅れて提示する上限、同じ目標に重なったとき、1 回 1 枚、未来のフレームだけなら描かない、世代の取り直し) は `tests/test_playout/buffer.rs` と `pbt/tests/prop_playout/buffer.rs` で固定している。選択の目標 (表示周期の実測への追随) と、描画を観測した分だけを実績に記録することは `cargo test -p moq-sub` で固定している
- 同じ素材・同じリレーで qmux と QUIC 直接、映像あり / 映像なし、subgroup / datagram を比較する。qmux だけで `underruns` が増え、QMux の破棄警告が出ていなければ TCP の再送待ちによるバーストが原因である
- moq-pub の `--target-latency` を 300〜500 ms にして症状が消えるかを見る。消えるなら遅延スパイクに対して目標遅延が不足している
- `e2e-tests` は実リレーを必要とし、`#[ignore]` を付けて `MOQT_E2E_URL` を設定したときだけ実行する。確認できるのは、publish と subscribe の往復、音声 datagram の受信と MP4 保存、プロセスの異常終了が無いこと、新しい計器がログに出て削除した項目が出ないことである。聴感、実出力遅延、A/V のリップシンク、Bluetooth と OS のミキサー、qmux 経由の TCP 再送バースト、Wi-Fi の周期的な停滞、長時間の傾向は確認できない

## 未確認のこと

- 音声だけの購読で 50 ms を下限に戻したときの穴と段差。旧 140 ms の A/B と同じ条件 (Wi-Fi、0.5 秒ごとに 100 ms 前後の停滞) で取り直していない
- `audio_min_delay_us` を 50 ms 未満にしたときの穴と段差。moqt-js は実 Chromium の直接出力で下限 10 ms を測ったが、moqt-rs は `raw_player` の FIFO と SDL のバッファが加わるため、同じ値をそのまま採用できるかは未確認である
- 固定 5 ms 先行の撤去が実機の A/V のずれに与える影響。共有する基準が同じ表示時刻へ揃うことは単体テストで固定したが、実リレーでの `skew_us` の分布は取り直していない
- FIFO 出力での先行量の意味。予算へ先行量を入れても、実際の開始はキューの終端で決まる。目標と実際の差は残差として計器に出るが、残差を映像の補正にも使っているため、二重補正にならないことの実機確認が要る
- 出力側の欠落判定と補間の適用層 (AudioWorklet 相当) が moqt-rs に無いこと。DSP (`stretch` / `expansion` / `predictor` / `splice_fade` / `gap`) は example の到着側の材料化から使っているが、出力コールバックで実欠落を検出して補間する層は無い。呼び出し側が出力を持つ場合の API として使う想定である
- 映像の表示の遅れが 2 秒窓の百分位へ直接追従することの副作用。フレーム間隔の推定が不安定な開始直後に目標が跳ねないか
- 時間伸長を入れたときの聴感と、圧縮との往復が増えないこと
- 5 / 10 ms の短いフレームで、履歴と連結した周期解析が実リレーの音声でも圧縮・伸長を成立させること。単体テストは 400 Hz / 200 Hz の連続波でだけ確認している
- `playout::resample` の通過帯域・阻止帯域・群遅延を、実リレーの音声と複数のレートの組み合わせで確認すること。単体テストは 96 → 48 kHz の正弦波と DC でだけ確認している
- 欠落の直後で直前の実音の履歴が 30 ms 未満になるときの補間の品質。moqt-js のリングバッファとの差は未確認である
- 1 ループの実測周期 (sleep 1 ms と処理時間の合計)。1〜3 ms は推定である
- 投入から描画を観測するまでの遅れの実測値
- `present` が戻ってから画面に出るまでの残差。SDL とコンポジタの実装依存である
- 120 fps 相当で同じ許容幅に 2 枚のフレームが入る頻度
- 実リレーでの `skew_us` の実分布
- 実リレーでの 90 秒以上の長時間の傾向
